// PoC evaluator — this implementation will almost certainly be rewritten.
// Implement the simplest correct thing; do not over-engineer.

pub(crate) mod builtins;
mod call;
mod display;
mod lvalue;
mod pattern;
mod type_of;

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::ast::{BinOp, Literal, Param, Span, TypeExpr, UnaryOp};
use crate::error::{FrameInfo, MetelError, RuntimeErrorCode};
use crate::typeinference::TypeCtx;

thread_local! {
    static CALL_STACK: RefCell<Vec<FrameInfo>> = const { RefCell::new(Vec::new()) };
    static PROFILER: RefCell<Option<ProfilerState>> = const { RefCell::new(None) };
}

pub(super) fn push_frame(fn_name: String, call_site: Span) {
    profiler_enter(&fn_name);
    CALL_STACK.with(|s| s.borrow_mut().push(FrameInfo { fn_name, call_site }));
}

pub(super) fn pop_frame() {
    profiler_exit();
    CALL_STACK.with(|s| {
        s.borrow_mut().pop();
    });
}

fn snapshot_stack() -> Vec<FrameInfo> {
    CALL_STACK.with(|s| s.borrow().clone())
}

pub(super) fn attach_stack(err: MetelError) -> MetelError {
    err.with_stack(snapshot_stack())
}
use crate::ast::Block;
use crate::elaborator::ElaboratedModuleGraph;
use crate::symbols::SymbolId;
use crate::typed_ast::{
    FunBody, MethodDispatch, ResolvedImportRef, TypedBlock, TypedDecl, TypedExpr, TypedForInit,
    TypedProgram, TypedStmt,
};

// ── Runtime values ────────────────────────────────────────────────────────────

/// One step in a `MutFieldPointer` path.
#[derive(Debug, Clone)]
pub enum PathSegment {
    Field(String),
    TupleIndex(usize),
    ArrayIndex(usize),
}

#[derive(Debug, Clone)]
pub enum Value {
    // ── Primitive types ───────────────────────────────────────────────────────
    I64(i64),
    /// Sized signed integers.
    I8(i8),
    I16(i16),
    I32(i32),
    /// Sized unsigned integers.
    U8(u8),
    U16(u16),
    U32(u32),
    U64(u64),
    F64(f64),
    /// 32-bit float.
    F32(f32),
    Char(char),
    Boolean(bool),
    Str(String),
    Unit,
    // ── Compound types ────────────────────────────────────────────────────────
    Tuple(Vec<Value>),
    Array(Rc<RefCell<Vec<Value>>>),
    Struct {
        name: String,
        /// Stable identity of the struct's declaration (METEL-185 / ADR-0041). Lets
        /// method dispatch resolve the receiver to a type `SymbolId` instead of its
        /// surface name, so two modules' same-named structs never collide. `None`
        /// for values built without resolver context (the single-program path) and
        /// for builtins not yet threaded; dispatch falls back to the name then.
        type_id: Option<SymbolId>,
        fields: HashMap<String, Value>,
    },
    // Perhaps<T> and Result<T,E> use Value::Enum like all other enums. See ADR-0028.
    Enum {
        name: String,
        /// Stable identity of the enum's declaration. See `Struct::type_id`.
        type_id: Option<SymbolId>,
        variant: String,
        fields: HashMap<String, Value>,
    },
    Callable(RuntimeCallable),
    /// Read-only pointer to a named binding cell.
    Pointer(Rc<RefCell<Value>>),
    /// Writable pointer to a named binding cell.
    MutPointer(Rc<RefCell<Value>>),
    /// Fat mutable pointer for sub-element lvalue paths (RFC-0045).
    /// `root` is the binding cell; `path` navigates to the leaf.
    MutFieldPointer {
        root: Rc<RefCell<Value>>,
        path: Vec<PathSegment>,
    },
}

/// The body of a closure — either a fully type-checked block (monomorphic) or the
/// original untyped block (generic / let-polymorphic). The evaluator dispatches on
/// this to choose between `eval_block` and `eval_untyped_block`.
#[derive(Debug, Clone)]
pub enum ClosureBody {
    Typed(TypedBlock),
    Untyped(Block),
}

#[derive(Debug, Clone)]
pub enum RuntimeCallable {
    Closure(Rc<ClosureValue>),
    Intrinsic {
        label: String,
        fun: fn(Vec<Value>, &Span) -> Result<Value, MetelError>,
    },
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct EvaluationOptions {
    pub collect_profile: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct EvaluationReport {
    pub profile: Option<EvaluatorProfile>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct EvaluatorProfile {
    pub functions: Vec<FunctionProfile>,
    pub edges: Vec<CallEdgeProfile>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FunctionProfile {
    pub function: String,
    pub calls: u64,
    pub inclusive_ns: u64,
    pub self_ns: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CallEdgeProfile {
    pub caller: Option<String>,
    pub callee: String,
    pub calls: u64,
    pub inclusive_ns: u64,
}

#[derive(Debug, Default)]
struct ProfilerState {
    functions: HashMap<String, FunctionAccumulator>,
    edges: HashMap<(Option<String>, String), EdgeAccumulator>,
    active_frames: Vec<ActiveProfilerFrame>,
}

#[derive(Debug, Default)]
struct FunctionAccumulator {
    calls: u64,
    inclusive_ns: u64,
    self_ns: u64,
}

#[derive(Debug, Default)]
struct EdgeAccumulator {
    calls: u64,
    inclusive_ns: u64,
}

#[derive(Debug)]
struct ActiveProfilerFrame {
    function: String,
    started_at: Instant,
    child_time: Duration,
}

impl ProfilerState {
    fn into_profile(self) -> EvaluatorProfile {
        let functions = self
            .functions
            .into_iter()
            .map(|(function, acc)| FunctionProfile {
                function,
                calls: acc.calls,
                inclusive_ns: acc.inclusive_ns,
                self_ns: acc.self_ns,
            })
            .collect::<Vec<_>>();
        let edges = self
            .edges
            .into_iter()
            .map(|((caller, callee), acc)| CallEdgeProfile {
                caller,
                callee,
                calls: acc.calls,
                inclusive_ns: acc.inclusive_ns,
            })
            .collect::<Vec<_>>();

        let mut function_map: BTreeMap<String, FunctionProfile> = BTreeMap::new();
        for function in functions {
            function_map.insert(function.function.clone(), function);
        }
        let mut edges = edges;
        edges.sort_by(|lhs, rhs| {
            lhs.caller
                .cmp(&rhs.caller)
                .then(lhs.callee.cmp(&rhs.callee))
        });

        EvaluatorProfile {
            functions: function_map.into_values().collect(),
            edges,
        }
    }
}

fn reset_runtime_state(collect_profile: bool) {
    CALL_STACK.with(|s| s.borrow_mut().clear());
    PROFILER.with(|profiler| {
        *profiler.borrow_mut() = collect_profile.then(ProfilerState::default);
    });
}

fn finish_profile() -> Option<EvaluatorProfile> {
    PROFILER.with(|profiler| {
        profiler
            .borrow_mut()
            .take()
            .map(ProfilerState::into_profile)
    })
}

pub(super) fn profiler_enter(function: &str) {
    PROFILER.with(|profiler| {
        let mut profiler = profiler.borrow_mut();
        let Some(state) = profiler.as_mut() else {
            return;
        };

        let caller = state
            .active_frames
            .last()
            .map(|frame| frame.function.clone());
        state
            .functions
            .entry(function.to_string())
            .or_default()
            .calls += 1;
        state
            .edges
            .entry((caller, function.to_string()))
            .or_default()
            .calls += 1;
        state.active_frames.push(ActiveProfilerFrame {
            function: function.to_string(),
            started_at: Instant::now(),
            child_time: Duration::ZERO,
        });
    });
}

pub(super) fn profiler_exit() {
    PROFILER.with(|profiler| {
        let mut profiler = profiler.borrow_mut();
        let Some(state) = profiler.as_mut() else {
            return;
        };
        let Some(frame) = state.active_frames.pop() else {
            return;
        };

        let elapsed = frame.started_at.elapsed();
        let inclusive_ns = duration_ns(elapsed);
        let self_ns = duration_ns(elapsed.saturating_sub(frame.child_time));

        if let Some(acc) = state.functions.get_mut(&frame.function) {
            acc.inclusive_ns += inclusive_ns;
            acc.self_ns += self_ns;
        }

        let caller = state
            .active_frames
            .last()
            .map(|active| active.function.clone());
        if let Some(acc) = state.edges.get_mut(&(caller, frame.function.clone())) {
            acc.inclusive_ns += inclusive_ns;
        }
        if let Some(parent) = state.active_frames.last_mut() {
            parent.child_time += elapsed;
        }
    });
}

fn duration_ns(duration: Duration) -> u64 {
    duration.as_nanos().min(u64::MAX as u128) as u64
}

#[derive(Debug, Clone, Default)]
pub struct RuntimeModuleEntry {
    values: HashMap<String, Value>,
}

#[derive(Debug, Clone, Default)]
pub struct RuntimeTypeEntry {
    associated_values: HashMap<String, RuntimeMethod>,
    inherent_methods: HashMap<String, RuntimeMethod>,
    aspect_impls: Vec<RuntimeAspectImpl>,
}

#[derive(Debug, Clone, Default)]
pub struct RuntimeAspectImpl {
    aspect_name: String,
    /// Stable identity of the aspect; `None` when aspect was registered via the old
    /// string-only path (builtins / single-module pipeline without name resolver).
    aspect_id: Option<SymbolId>,
    type_args: Vec<String>,
    methods: HashMap<String, RuntimeMethod>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeTypeRef {
    Named(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RuntimeTypePattern {
    Str,
    Array,
    Primitive(String),
}

#[derive(Debug, Clone, Default)]
pub struct RuntimeSignature {
    #[allow(dead_code)] // stored for future diagnostics/reflection and System F transition work
    pub params: Vec<RuntimeTypeRef>,
    #[allow(dead_code)] // stored for future diagnostics/reflection and System F transition work
    pub ret: Option<RuntimeTypeRef>,
}

#[derive(Debug, Clone)]
pub struct RuntimeMethod {
    #[allow(dead_code)] // stored for diagnostics/debugging; not used for structural lookup
    pub label: String,
    pub receiver: Option<crate::ast::ReceiverKind>,
    #[allow(dead_code)] // stored for future diagnostics/reflection and System F transition work
    pub signature: RuntimeSignature,
    pub body: RuntimeCallable,
}

#[derive(Debug, Clone, Default)]
pub struct RuntimeRegistry {
    modules: HashMap<Vec<String>, RuntimeModuleEntry>,
    /// Struct/enum type entries keyed by the type's stable `SymbolId` (METEL-185 /
    /// ADR-0041). Instance method dispatch resolves the receiver `Value` to this id
    /// directly, so two modules' same-named types never collide.
    types: HashMap<SymbolId, RuntimeTypeEntry>,
    /// Surface type name → `SymbolId`, the single name→id resolution step (ADR-0041)
    /// for sites that only have a name: static-member access (`List::new`), `From`
    /// targets, and host-built values whose `type_id` was not threaded. Not a method
    /// lookup — it only maps a name to an id, which then keys `types`.
    type_ids: HashMap<String, SymbolId>,
    pattern_methods: HashMap<RuntimeTypePattern, HashMap<String, RuntimeMethod>>,
    /// Callables dispatched by stable SymbolId rather than by name — overloaded
    /// free-function definitions (METEL-180) and ordinary top-level functions
    /// (METEL-187), whose surface name cannot always identify a single definition.
    symbol_values: HashMap<SymbolId, Value>,
    /// Every top-level (module-level) `let`/`mut`'s `def_id` (ADR-0042), populated
    /// once before Pass 1 from the whole program's declarations. Unlike a top-level
    /// `fn`'s `def_id` (always registered in `symbol_values` by Pass 1b, before any
    /// user code runs), a `let`/`mut`'s value is only registered when its Pass 2
    /// initializer actually executes — so a `Call::callee_id` miss on one of these ids
    /// can be a legitimate "used before it was defined" runtime error, not a bug. This
    /// set is exactly what distinguishes that case from an unconditional internal
    /// error (see the `TypedExpr::Call` handling in `eval_expr`).
    let_mut_def_ids: std::collections::HashSet<SymbolId>,
}

type FieldWriteback = (Rc<RefCell<Value>>, Vec<String>, Rc<RefCell<Value>>);

impl RuntimeRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_symbol_value(&mut self, id: SymbolId, value: Value) {
        self.symbol_values.insert(id, value);
    }

    pub fn get_symbol_value(&self, id: SymbolId) -> Option<&Value> {
        self.symbol_values.get(&id)
    }

    /// Record that `id` belongs to a top-level `let`/`mut` (ADR-0042) — see the field
    /// doc on `let_mut_def_ids`.
    pub fn mark_let_mut_def_id(&mut self, id: SymbolId) {
        self.let_mut_def_ids.insert(id);
    }

    /// Whether `id` is a top-level `let`/`mut`'s identity, i.e. a `symbol_values` miss
    /// on it can be a legitimate "used before it was defined" runtime error rather
    /// than an internal bug.
    pub fn is_let_mut_def_id(&self, id: SymbolId) -> bool {
        self.let_mut_def_ids.contains(&id)
    }

    pub fn register_module_value(
        &mut self,
        module_path: impl Into<Vec<String>>,
        name: impl Into<String>,
        value: Value,
    ) {
        self.modules
            .entry(module_path.into())
            .or_default()
            .values
            .insert(name.into(), value);
    }

    pub fn register_std_core_value(&mut self, name: impl Into<String>, value: Value) {
        self.register_module_value(vec!["std".to_string(), "core".to_string()], name, value);
    }

    /// Get (or create) the type entry for `type_id`, recording the `type_name → id`
    /// resolution so name-only sites can resolve to this id later.
    fn type_entry_mut(&mut self, type_id: SymbolId, type_name: &str) -> &mut RuntimeTypeEntry {
        self.type_ids.insert(type_name.to_string(), type_id);
        self.types.entry(type_id).or_default()
    }

    /// Resolve a surface type name to its registered `SymbolId` (single resolution
    /// step; see [`RuntimeRegistry::type_ids`]).
    fn type_id_for_name(&self, type_name: &str) -> Option<SymbolId> {
        self.type_ids.get(type_name).copied()
    }

    pub fn register_type_value(
        &mut self,
        type_id: SymbolId,
        type_name: &str,
        name: impl Into<String>,
        value: RuntimeMethod,
    ) {
        self.type_entry_mut(type_id, type_name)
            .associated_values
            .insert(name.into(), value);
    }

    pub fn register_inherent_method(
        &mut self,
        type_id: SymbolId,
        type_name: &str,
        method_name: impl Into<String>,
        value: RuntimeMethod,
    ) {
        self.type_entry_mut(type_id, type_name)
            .inherent_methods
            .insert(method_name.into(), value);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn register_aspect_method(
        &mut self,
        type_id: SymbolId,
        type_name: &str,
        aspect_name: impl Into<String>,
        aspect_id: Option<SymbolId>,
        type_args: Vec<String>,
        method_name: impl Into<String>,
        value: RuntimeMethod,
    ) {
        let entry = self.type_entry_mut(type_id, type_name);
        let aspect_name = aspect_name.into();
        let method_name = method_name.into();
        if let Some(aspect_impl) = entry.aspect_impls.iter_mut().find(|aspect_impl| {
            aspect_impl.aspect_name == aspect_name && aspect_impl.type_args == type_args
        }) {
            // Update aspect_id if we now have one (a later registration may have the id).
            if aspect_impl.aspect_id.is_none() {
                aspect_impl.aspect_id = aspect_id;
            }
            aspect_impl.methods.insert(method_name, value);
            return;
        }

        let mut methods = HashMap::new();
        methods.insert(method_name, value);
        entry.aspect_impls.push(RuntimeAspectImpl {
            aspect_name,
            aspect_id,
            type_args,
            methods,
        });
    }

    /// Look up a method belonging to a specific aspect impl, by the aspect's stable
    /// `SymbolId`. Selection is purely id-based: builtin aspect impls are seeded with
    /// their `SYM_ASPECT_*` ids and user impls carry the elaboration-stamped id, so
    /// no surface-name fallback is needed (METEL-185 / ADR-0041).
    pub fn get_aspect_method_by_id(
        &self,
        type_id: SymbolId,
        aspect_id: SymbolId,
        method_name: &str,
    ) -> Option<RuntimeMethod> {
        self.types.get(&type_id)?.aspect_impls.iter().rev().find_map(|ai| {
            if ai.aspect_id == Some(aspect_id) {
                ai.methods
                    .get(method_name)
                    .cloned()
                    .filter(|m| m.receiver.is_some())
            } else {
                None
            }
        })
    }

    pub fn register_pattern_method(
        &mut self,
        pattern: RuntimeTypePattern,
        method_name: impl Into<String>,
        value: RuntimeMethod,
    ) {
        self.pattern_methods
            .entry(pattern)
            .or_default()
            .insert(method_name.into(), value);
    }

    pub fn get_module_value(&self, module_path: &[String], name: &str) -> Option<Value> {
        self.modules.get(module_path)?.values.get(name).cloned()
    }

    pub fn get_type_value(&self, type_name: &str, name: &str) -> Option<Value> {
        let type_entry = self.types.get(&self.type_id_for_name(type_name)?)?;
        type_entry
            .associated_values
            .get(name)
            .map(|method| Value::Callable(method.body.clone()))
            .or_else(|| {
                type_entry
                    .aspect_impls
                    .iter()
                    .rev()
                    .find_map(|aspect_impl| {
                        aspect_impl
                            .methods
                            .get(name)
                            .filter(|method| method.receiver.is_none())
                            .map(|method| Value::Callable(method.body.clone()))
                    })
            })
    }

    pub fn get_inherent_method(
        &self,
        type_id: SymbolId,
        method_name: &str,
    ) -> Option<RuntimeMethod> {
        self.types
            .get(&type_id)?
            .inherent_methods
            .get(method_name)
            .cloned()
            .filter(|method| method.receiver.is_some())
    }

    pub fn get_regular_method(
        &self,
        type_id: SymbolId,
        method_name: &str,
    ) -> Option<RuntimeMethod> {
        self.get_inherent_method(type_id, method_name).or_else(|| {
            self.types
                .get(&type_id)?
                .aspect_impls
                .iter()
                .rev()
                .find_map(|aspect_impl| {
                    aspect_impl
                        .methods
                        .get(method_name)
                        .cloned()
                        .filter(|method| method.receiver.is_some())
                })
        })
    }

    pub fn get_method_for_value(&self, value: &Value, method_name: &str) -> Option<RuntimeMethod> {
        self.resolve_value_type_id(value)
            .and_then(|type_id| self.get_regular_method(type_id, method_name))
            .or_else(|| {
                runtime_type_pattern(value).and_then(|pattern| {
                    self.pattern_methods
                        .get(&pattern)?
                        .get(method_name)
                        .cloned()
                        .filter(|method| method.receiver.is_some())
                })
            })
    }

    /// Resolve a receiver `Value` to its type's `SymbolId` for method dispatch:
    /// the value's carried `type_id` when present (cross-module correct), else the
    /// name→id index (host-built values, primitives). `None` for values with no
    /// type entry (Array/Tuple/etc., which dispatch via `pattern_methods`).
    fn resolve_value_type_id(&self, value: &Value) -> Option<SymbolId> {
        match value {
            Value::Struct { type_id, name, .. } | Value::Enum { type_id, name, .. } => {
                type_id.or_else(|| self.type_id_for_name(name))
            }
            _ => runtime_type_name(value).and_then(|name| self.type_id_for_name(name)),
        }
    }

    pub fn get_from_method(&self, target: &str, source: &str) -> Option<RuntimeMethod> {
        let target_id = self.type_id_for_name(target)?;
        self.types
            .get(&target_id)?
            .aspect_impls
            .iter()
            .rev()
            .find_map(|aspect_impl| {
                (aspect_impl.aspect_name == "From"
                    && aspect_impl.type_args.len() == 1
                    && aspect_impl.type_args[0] == source)
                    .then(|| aspect_impl.methods.get("from").cloned())
                    .flatten()
            })
            .or_else(|| {
                self.types
                    .get(&target_id)?
                    .associated_values
                    .get("from")
                    .cloned()
                    .or_else(|| self.inherent_method_without_receiver(target_id, "from"))
            })
    }

    fn inherent_method_without_receiver(
        &self,
        type_id: SymbolId,
        method_name: &str,
    ) -> Option<RuntimeMethod> {
        self.types
            .get(&type_id)?
            .inherent_methods
            .get(method_name)
            .cloned()
            .filter(|method| method.receiver.is_none())
    }

    pub fn resolve_module_export(&self, module_path: &[String], local_name: &str) -> Option<Value> {
        self.get_module_value(module_path, local_name).or_else(|| {
            let mut segments = local_name.split("::");
            let type_name = segments.next()?;
            let member_name = segments.next()?;
            if segments.next().is_some() {
                return None;
            }
            self.get_type_value(type_name, member_name)
        })
    }

    pub fn resolve_path_value(&self, segments: &[String]) -> Option<Value> {
        if segments.len() >= 3 {
            let module_path = segments[..2].to_vec();
            let local_name = segments[2..].join("::");
            if let Some(value) = self.resolve_module_export(&module_path, &local_name) {
                return Some(value);
            }
        }

        if segments.len() == 2 {
            return self.get_type_value(&segments[0], &segments[1]);
        }

        None
    }
}

#[derive(Debug, Clone)]
pub struct ClosureValue {
    pub name: Option<String>,
    pub params: Vec<Param>,
    pub body: ClosureBody,
    pub captured: Environment,
    /// Present only when `body` is `ClosureBody::Untyped` (generic function). Provides
    /// the type context for construction-at-call-time so the untyped path is not needed.
    pub type_ctx: Option<std::rc::Rc<TypeCtx>>,
    /// The concrete function type of this closure, if known. Used by `value_to_type` to
    /// recover the closure's parameter/return types when it is passed as a generic argument.
    pub fun_type: Option<crate::types::Type>,
}

/// Deep-clone a value so that arrays get independent copies.
/// Tuples, structs, and enums are recursed into so that nested arrays are also copied.
/// All other value kinds contain no shared mutable state and can be cloned shallowly.
fn deep_clone_value(v: Value) -> Value {
    match v {
        Value::Array(rc) => {
            let cloned: Vec<Value> = rc.borrow().iter().cloned().map(deep_clone_value).collect();
            Value::Array(Rc::new(RefCell::new(cloned)))
        }
        Value::Tuple(items) => Value::Tuple(items.into_iter().map(deep_clone_value).collect()),
        Value::Struct {
            name,
            type_id,
            fields,
        } => Value::Struct {
            name,
            type_id,
            fields: fields
                .into_iter()
                .map(|(k, v)| (k, deep_clone_value(v)))
                .collect(),
        },
        Value::Enum {
            name,
            type_id,
            variant,
            fields,
        } => Value::Enum {
            name,
            type_id,
            variant,
            fields: fields
                .into_iter()
                .map(|(k, v)| (k, deep_clone_value(v)))
                .collect(),
        },
        other => other,
    }
}

/// Walk a `PathSegment` path into `root`, returning a clone of the leaf value.
fn read_path(root: &Value, path: &[PathSegment], span: &Span) -> Result<Value, MetelError> {
    let mut cur = root.clone();
    for seg in path {
        cur = match (seg, cur) {
            (PathSegment::Field(f), Value::Struct { fields, .. } | Value::Enum { fields, .. }) => {
                fields.get(f.as_str()).cloned().ok_or_else(|| {
                    MetelError::panic(
                        RuntimeErrorCode::R0008,
                        format!("fat pointer: no field `{f}`"),
                        span,
                    )
                })?
            }
            (PathSegment::TupleIndex(i), Value::Tuple(elems)) => {
                elems.get(*i).cloned().ok_or_else(|| {
                    MetelError::panic(
                        RuntimeErrorCode::R0008,
                        format!("fat pointer: tuple index {i} out of bounds"),
                        span,
                    )
                })?
            }
            (PathSegment::ArrayIndex(i), Value::Array(rc)) => {
                rc.borrow().get(*i).cloned().ok_or_else(|| {
                    MetelError::panic(
                        RuntimeErrorCode::R0004,
                        format!("fat pointer: array index {i} out of bounds"),
                        span,
                    )
                })?
            }
            _ => {
                return Err(MetelError::internal(
                    "fat pointer path: segment type mismatch",
                ))
            }
        };
    }
    Ok(cur)
}

/// Walk a `PathSegment` path into `root` and write `new_val` at the leaf.
fn write_path(
    root: &mut Value,
    path: &[PathSegment],
    new_val: Value,
    span: &Span,
) -> Result<(), MetelError> {
    if path.is_empty() {
        *root = new_val;
        return Ok(());
    }
    match (&path[0], root) {
        (PathSegment::Field(f), Value::Struct { fields, .. } | Value::Enum { fields, .. }) => {
            let child = fields.get_mut(f.as_str()).ok_or_else(|| {
                MetelError::panic(
                    RuntimeErrorCode::R0008,
                    format!("fat pointer: no field `{f}`"),
                    span,
                )
            })?;
            write_path(child, &path[1..], new_val, span)
        }
        (PathSegment::TupleIndex(i), Value::Tuple(elems)) => {
            let child = elems.get_mut(*i).ok_or_else(|| {
                MetelError::panic(
                    RuntimeErrorCode::R0008,
                    format!("fat pointer: tuple index {i} out of bounds"),
                    span,
                )
            })?;
            write_path(child, &path[1..], new_val, span)
        }
        (PathSegment::ArrayIndex(i), Value::Array(rc)) => {
            let mut borrow = rc.borrow_mut();
            let child = borrow.get_mut(*i).ok_or_else(|| {
                MetelError::panic(
                    RuntimeErrorCode::R0004,
                    format!("fat pointer: array index {i} out of bounds"),
                    span,
                )
            })?;
            write_path(child, &path[1..], new_val, span)
        }
        _ => Err(MetelError::internal(
            "fat pointer path: segment type mismatch during write",
        )),
    }
}

/// Like `Pointer`/`MutPointer` deref but also handles `MutFieldPointer` with a proper span.
fn deref_value(value: &Value, span: &Span) -> Result<Option<Value>, MetelError> {
    match value {
        Value::Pointer(rc) | Value::MutPointer(rc) => Ok(Some(rc.borrow().clone())),
        Value::MutFieldPointer { root, path } => Ok(Some(read_path(&root.borrow(), path, span)?)),
        _ => Ok(None),
    }
}

fn receiver_cell_from_value(value: &Value) -> Option<Rc<RefCell<Value>>> {
    match value {
        Value::Pointer(rc) | Value::MutPointer(rc) => Some(Rc::clone(rc)),
        _ => None,
    }
}

fn runtime_type_name(value: &Value) -> Option<&str> {
    match value {
        Value::Struct { name, .. } | Value::Enum { name, .. } => Some(name.as_str()),
        Value::I64(_) => Some("i64"),
        Value::I8(_) => Some("i8"),
        Value::I16(_) => Some("i16"),
        Value::I32(_) => Some("i32"),
        Value::U8(_) => Some("u8"),
        Value::U16(_) => Some("u16"),
        Value::U32(_) => Some("u32"),
        Value::U64(_) => Some("u64"),
        Value::F64(_) => Some("f64"),
        Value::F32(_) => Some("f32"),
        Value::Char(_) => Some("Char"),
        Value::Boolean(_) => Some("boolean"),
        Value::Str(_) => Some("String"),
        _ => None,
    }
}

fn runtime_type_pattern(value: &Value) -> Option<RuntimeTypePattern> {
    match value {
        Value::Str(_) => Some(RuntimeTypePattern::Str),
        Value::Array(_) => Some(RuntimeTypePattern::Array),
        Value::I64(_) => Some(RuntimeTypePattern::Primitive("i64".to_string())),
        Value::I8(_) => Some(RuntimeTypePattern::Primitive("i8".to_string())),
        Value::I16(_) => Some(RuntimeTypePattern::Primitive("i16".to_string())),
        Value::I32(_) => Some(RuntimeTypePattern::Primitive("i32".to_string())),
        Value::U8(_) => Some(RuntimeTypePattern::Primitive("u8".to_string())),
        Value::U16(_) => Some(RuntimeTypePattern::Primitive("u16".to_string())),
        Value::U32(_) => Some(RuntimeTypePattern::Primitive("u32".to_string())),
        Value::U64(_) => Some(RuntimeTypePattern::Primitive("u64".to_string())),
        Value::F64(_) => Some(RuntimeTypePattern::Primitive("f64".to_string())),
        Value::F32(_) => Some(RuntimeTypePattern::Primitive("f32".to_string())),
        Value::Char(_) => Some(RuntimeTypePattern::Primitive("Char".to_string())),
        Value::Boolean(_) => Some(RuntimeTypePattern::Primitive("boolean".to_string())),
        _ => None,
    }
}

fn runtime_type_key(ty: &TypeExpr) -> String {
    match ty {
        TypeExpr::Named(name, args) if args.is_empty() => name.clone(),
        TypeExpr::Named(name, args) => format!(
            "{name}<{}>",
            args.iter()
                .map(runtime_type_key)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        TypeExpr::Unit => "()".to_string(),
        TypeExpr::Tuple(items) => format!(
            "({})",
            items
                .iter()
                .map(runtime_type_key)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        TypeExpr::Array(inner) => format!("{}[]", runtime_type_key(inner)),
        TypeExpr::SizedArray(inner, size) => format!("[{}; {}]", runtime_type_key(inner), size),
        TypeExpr::Pointer(inner) => format!("*{}", runtime_type_key(inner)),
        TypeExpr::MutPointer(inner) => format!("*mut {}", runtime_type_key(inner)),
        TypeExpr::Fun(params, ret) => {
            let params = params
                .iter()
                .map(runtime_type_key)
                .collect::<Vec<_>>()
                .join(", ");
            match ret {
                Some(ret) => format!("fun({params}) -> {}", runtime_type_key(ret)),
                None => format!("fun({params})"),
            }
        }
        TypeExpr::ImplAspect { bound, .. } => format!("impl {}", runtime_type_key(bound)),
    }
}

fn runtime_type_ref(ty: &TypeExpr) -> RuntimeTypeRef {
    RuntimeTypeRef::Named(runtime_type_key(ty))
}

fn runtime_signature(
    params: impl IntoIterator<Item = TypeExpr>,
    ret: Option<TypeExpr>,
) -> RuntimeSignature {
    RuntimeSignature {
        params: params.into_iter().map(|ty| runtime_type_ref(&ty)).collect(),
        ret: ret.map(|ty| runtime_type_ref(&ty)),
    }
}

fn runtime_method_from_decl(
    label: String,
    method: &crate::typed_ast::TypedFunDecl,
    body: RuntimeCallable,
) -> RuntimeMethod {
    let receiver = method
        .params
        .first()
        .and_then(|param| param.receiver.clone());
    let params = method
        .params
        .iter()
        .filter_map(|param| {
            if param.receiver.is_some() {
                None
            } else {
                Some(
                    param
                        .type_ann
                        .clone()
                        .unwrap_or_else(|| TypeExpr::Named("_".to_string(), vec![])),
                )
            }
        })
        .collect::<Vec<_>>();
    let signature = runtime_signature(params, method.return_type.clone());

    RuntimeMethod {
        label,
        receiver,
        signature,
        body,
    }
}

fn std_core_lookup(name: &str, runtime: &RuntimeRegistry) -> Option<Value> {
    runtime.get_module_value(&["std".to_string(), "core".to_string()], name)
}

// For a FieldAccess receiver like `a.b.c`, returns:
//   (struct_cell, ["a","b","c"], leaf_cell)
// where struct_cell is the Rc for the root variable (pointer-followed if needed),
// the path encodes every field segment, and leaf_cell is a fresh Rc wrapping a clone
// of the leaf value.  After a &mut self call the caller writes leaf_cell's value back.
fn lvalue_field_cell(
    receiver: &crate::typed_ast::TypedExpr,
    env: &Environment,
) -> Option<FieldWriteback> {
    use crate::typed_ast::TypedExpr;
    fn walk_path(expr: &TypedExpr, path: &mut Vec<String>) -> Option<String> {
        match expr {
            TypedExpr::Ident(name, _, _) => Some(name.clone()),
            TypedExpr::FieldAccess { object, field, .. } => {
                let root = walk_path(object, path)?;
                path.push(field.clone());
                Some(root)
            }
            _ => None,
        }
    }
    let mut path = Vec::new();
    let root = walk_path(receiver, &mut path)?;

    let root_cell = env.get_rc(&root)?;
    let struct_cell = {
        let inner = match &*root_cell.borrow() {
            Value::Pointer(c) | Value::MutPointer(c) => Some(Rc::clone(c)),
            _ => None,
        };
        inner.unwrap_or(root_cell)
    };
    let leaf_val = {
        let borrowed = struct_cell.borrow();
        let mut cur: &Value = &borrowed;
        for seg in &path {
            match cur {
                Value::Struct { fields, .. } | Value::Enum { fields, .. } => {
                    cur = fields.get(seg.as_str())?;
                }
                _ => return None,
            }
        }
        cur.clone()
    };
    let leaf_cell = Rc::new(RefCell::new(leaf_val));
    Some((struct_cell, path, leaf_cell))
}

fn is_lvalue_path_typed(expr: &crate::typed_ast::TypedExpr) -> bool {
    use crate::typed_ast::TypedExpr;
    match expr {
        TypedExpr::Ident(..) => true,
        TypedExpr::FieldAccess { object, .. }
        | TypedExpr::TupleAccess { object, .. }
        | TypedExpr::Index { object, .. } => is_lvalue_path_typed(object),
        _ => false,
    }
}

/// Recursively walk a typed lvalue path, collecting `PathSegment`s.
/// Returns the root binding name and the full segment list (root-to-leaf order).
fn build_mut_path(
    expr: &TypedExpr,
    env: &mut Environment,
    runtime: &RuntimeRegistry,
    span: &Span,
) -> Result<(String, Vec<PathSegment>), MetelError> {
    match expr {
        TypedExpr::Ident(name, _, _) => Ok((name.clone(), vec![])),
        TypedExpr::FieldAccess { object, field, .. } => {
            let (root, mut path) = build_mut_path(object, env, runtime, span)?;
            path.push(PathSegment::Field(field.clone()));
            Ok((root, path))
        }
        TypedExpr::TupleAccess { object, index, .. } => {
            let (root, mut path) = build_mut_path(object, env, runtime, span)?;
            path.push(PathSegment::TupleIndex(*index));
            Ok((root, path))
        }
        TypedExpr::Index { object, index, .. } => {
            let (root, mut path) = build_mut_path(object, env, runtime, span)?;
            let idx_val = eval_expr(index, env, runtime)?.into_value();
            let i = match idx_val {
                Value::I64(n) if n >= 0 => n as usize,
                Value::U64(n) => n as usize,
                _ => {
                    return Err(MetelError::panic(
                        RuntimeErrorCode::R0004,
                        "&mut: array index must be a non-negative integer",
                        span,
                    ))
                }
            };
            path.push(PathSegment::ArrayIndex(i));
            Ok((root, path))
        }
        _ => Err(MetelError::internal("build_mut_path: not a lvalue path")),
    }
}

// ── Control flow signals ──────────────────────────────────────────────────────

/// Returned by evaluation functions to handle non-local control flow.
/// Regular expression evaluation returns Signal::Value.
#[derive(Debug)]
pub enum Signal {
    Value(Value),
    Return(Value),
    Break(Value), // carries value for `loop { break expr; }`
    Continue,
}

impl Signal {
    /// Extract the inner `Value`, consuming the signal.
    /// Panics for non-Value signals — callers that need the full signal must match directly.
    pub fn into_value(self) -> Value {
        match self {
            Signal::Value(v) => v,
            other => panic!("Signal::into_value called on non-Value signal: {other:?}"),
        }
    }
}

// ── Environment ───────────────────────────────────────────────────────────────

/// Lexically-scoped environment — a stack of hashmaps.
/// Runtime storage stays cell-backed, but closure capture chooses whether to
/// clone cells by value (`capture_clone`) or share them explicitly (`define_rc`,
/// pointers, reference receivers).
#[derive(Debug, Clone)]
pub struct Environment {
    scopes: Vec<HashMap<String, Rc<RefCell<Value>>>>,
    /// Type context for construction-at-call-time of generic closures. Set once per module
    /// in `run_passes`; shared via `Rc` so cloning the environment is cheap.
    pub type_ctx: Option<std::rc::Rc<TypeCtx>>,
}

impl Default for Environment {
    fn default() -> Self {
        Self::new()
    }
}

impl Environment {
    pub fn new() -> Self {
        Self {
            scopes: vec![HashMap::new()],
            type_ctx: None,
        }
    }

    pub fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    pub fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    /// Define a new binding in the current scope.
    /// Arrays are deep-cloned so each binding has an independent copy.
    pub fn define(&mut self, name: &str, value: Value) {
        let cell = Rc::new(RefCell::new(deep_clone_value(value)));
        self.scopes
            .last_mut()
            .unwrap()
            .insert(name.to_string(), cell);
    }

    pub fn define_rc(&mut self, name: &str, cell: Rc<RefCell<Value>>) {
        self.scopes
            .last_mut()
            .unwrap()
            .insert(name.to_string(), cell);
    }

    /// Look up a binding, searching from innermost to outermost scope.
    pub fn get(&self, name: &str) -> Option<Value> {
        for scope in self.scopes.iter().rev() {
            if let Some(cell) = scope.get(name) {
                return Some(cell.borrow().clone());
            }
        }
        None
    }

    /// Assign to an existing binding anywhere in the scope chain.
    /// Arrays are deep-cloned so each binding has an independent copy.
    pub fn set(&self, name: &str, value: Value) -> bool {
        for scope in self.scopes.iter().rev() {
            if let Some(cell) = scope.get(name) {
                *cell.borrow_mut() = deep_clone_value(value);
                return true;
            }
        }
        false
    }

    /// Return the Rc for a binding (used by closures to share mutable state).
    pub fn get_rc(&self, name: &str) -> Option<Rc<RefCell<Value>>> {
        for scope in self.scopes.iter().rev() {
            if let Some(cell) = scope.get(name) {
                return Some(Rc::clone(cell));
            }
        }
        None
    }

    pub fn capture_clone(&self) -> Self {
        let scopes = self
            .scopes
            .iter()
            .map(|scope| {
                scope
                    .iter()
                    .map(|(name, cell)| {
                        let cloned = deep_clone_value(cell.borrow().clone());
                        (name.clone(), Rc::new(RefCell::new(cloned)))
                    })
                    .collect()
            })
            .collect();
        Self {
            scopes,
            type_ctx: self.type_ctx.clone(),
        }
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

/// Evaluate a typed module graph produced by `check_graph`.
///
/// Each module is initialised in its own `Environment` seeded with builtins,
/// then cross-linked via the `imported_names` table populated by `check_graph`.
/// Modules are processed in topological order (dependencies before dependents).
/// See ADR-0029 for the isolation design and ADR-0019 for the superseded flat-merge approach.
pub fn evaluate_graph(elaborated: ElaboratedModuleGraph) -> Result<(), MetelError> {
    evaluate_graph_with_options(elaborated, EvaluationOptions::default()).map(|_| ())
}

pub fn evaluate_graph_with_options(
    elaborated: ElaboratedModuleGraph,
    options: EvaluationOptions,
) -> Result<EvaluationReport, MetelError> {
    let graph = elaborated.0;
    reset_runtime_state(options.collect_profile);
    let mut runtime = builtins::runtime_registry();

    // module_envs: path → fully initialised Environment.
    // Built incrementally; later modules can look up values from earlier ones.
    let mut module_envs: HashMap<Vec<String>, Environment> = HashMap::new();

    let root_path = graph
        .modules
        .last()
        .map(|m| m.module_path.clone())
        .unwrap_or_default();

    for module in graph.modules {
        let mut env = Environment::new();

        // Seed names imported from already-initialised dependency modules.
        for (local_name, import_ref) in &module.imported_names {
            let ResolvedImportRef {
                source_module,
                canonical_name,
                ..
            } = import_ref;
            if let Some(src_env) = module_envs.get(source_module) {
                if let Some(val) = src_env.get(canonical_name) {
                    env.define(local_name, val);
                }
            } else if let Some(val) = runtime.get_module_value(source_module, canonical_name) {
                env.define(local_name, val);
            }
        }

        // Build type context for construction-at-call-time of generic function bodies.
        let type_ctx = std::rc::Rc::new(TypeCtx {
            scheme_env: module.scheme_env.clone(),
            registry: graph.type_registry.clone(),
        });

        // Run the standard 3-pass + alias evaluation on this module's decls.
        run_passes(
            &module.decls,
            &module.import_aliases,
            &mut env,
            &mut runtime,
            Some(type_ctx),
        )?;

        module_envs.insert(module.module_path, env);
    }

    // Run main() from the root module's environment.
    let dummy = Span {
        start: 0,
        end: 0,
        filename: "<program>".to_string(),
        line: 0,
        col: 0,
    };
    let env = module_envs.get_mut(&root_path).ok_or_else(|| {
        MetelError::panic(RuntimeErrorCode::R0001, "root module not found", &dummy)
    })?;
    let result = run_main(env, &runtime);
    let profile = finish_profile();
    result?;
    Ok(EvaluationReport { profile })
}

/// Run the standard 3-pass evaluation on `decls` into `env`.
///
/// Pass 1a: placeholder bindings so closures can capture each other's Rc.
/// Pass 1b: replace placeholders with real closures ("ties the knot").
/// Alias registration: bind aliased import names after closures exist.
/// Pass 2: evaluate top-level let/mut/stmt declarations in order.
///
/// `type_ctx` must be set on `env` before calling so that generic function bodies
/// capture it for construction-at-call-time.
fn run_passes(
    decls: &TypedProgram,
    aliases: &std::collections::HashMap<String, String>,
    env: &mut Environment,
    runtime: &mut RuntimeRegistry,
    type_ctx: Option<std::rc::Rc<TypeCtx>>,
) -> Result<(), MetelError> {
    env.type_ctx = type_ctx.clone();
    // Pass 0 (ADR-0042): record this module's top-level let/mut identities before
    // anything else runs, so a later `Call::callee_id` miss on one of them can be
    // recognized as "not registered yet" rather than an internal-error bug.
    for decl in decls {
        match decl {
            TypedDecl::Let(d) => {
                if let Some(id) = d.def_id {
                    runtime.mark_let_mut_def_id(id);
                }
            }
            TypedDecl::Mut(d) => {
                if let Some(id) = d.def_id {
                    runtime.mark_let_mut_def_id(id);
                }
            }
            _ => {}
        }
    }
    // Pass 1a. Overloaded definitions (symbol_id set) are dispatched through
    // the runtime's symbol registry, never by name — no env binding for them.
    for decl in decls {
        if let TypedDecl::Fun(f) = decl {
            if f.symbol_id.is_none() {
                env.define(&f.name, Value::Unit);
            }
        }
    }

    // Pass 1b
    for decl in decls {
        match decl {
            TypedDecl::Fun(f) => {
                // Native functions bind directly to their host implementation.
                // Overloaded ones (symbol_id set) go to the symbol registry.
                if let FunBody::Native(key) = &f.body {
                    let value =
                        Value::Callable(crate::evaluator::builtins::native_host_impl(*key));
                    match f.symbol_id {
                        Some(id) => runtime.register_symbol_value(id, value),
                        None => {
                            // Ordinary top-level fn: bind by name (first-class uses) and,
                            // when it has a stable identity, also register it by SymbolId so
                            // direct calls dispatch through `callee_id` (METEL-187).
                            if let Some(id) = f.def_id {
                                runtime.register_symbol_value(id, value.clone());
                            }
                            env.set(&f.name, value);
                        }
                    }
                    continue;
                }
                let (body, ctx) = match &f.body {
                    FunBody::Typed(b) => (ClosureBody::Typed(b.clone()), None),
                    FunBody::Generic(b) => (ClosureBody::Untyped(b.clone()), env.type_ctx.clone()),
                    FunBody::Native(_) => unreachable!("native handled above"),
                };
                let captured = env.clone();
                let value = Value::Callable(RuntimeCallable::Closure(Rc::new(ClosureValue {
                    name: Some(f.name.clone()),
                    params: f.params.clone(),
                    body,
                    captured,
                    type_ctx: ctx,
                    fun_type: None,
                })));
                match f.symbol_id {
                    Some(id) => runtime.register_symbol_value(id, value),
                    None => {
                        if let Some(id) = f.def_id {
                            runtime.register_symbol_value(id, value.clone());
                        }
                        env.set(&f.name, value);
                    }
                }
            }
            TypedDecl::Impl(impl_block) => {
                if let crate::ast::TypeExpr::Named(type_name, _) = &impl_block.target_type {
                    // The target type's stable id keys the runtime registry (METEL-185).
                    // Fall back to its builtin id when resolution wasn't available.
                    let Some(target_id) = impl_block
                        .target_type_id
                        .or_else(|| builtins::builtin_type_id(type_name))
                    else {
                        continue;
                    };
                    for method in &impl_block.methods {
                        // Native methods bind to their host implementation;
                        // others wrap their (typed/generic) body in a closure.
                        let body_callable = match &method.body {
                            FunBody::Native(key) => {
                                crate::evaluator::builtins::native_host_impl(*key)
                            }
                            FunBody::Typed(b) => RuntimeCallable::Closure(Rc::new(ClosureValue {
                                name: Some(method.name.clone()),
                                params: method.params.clone(),
                                body: ClosureBody::Typed(b.clone()),
                                captured: env.clone(),
                                type_ctx: None,
                                fun_type: None,
                            })),
                            FunBody::Generic(b) => RuntimeCallable::Closure(Rc::new(ClosureValue {
                                name: Some(method.name.clone()),
                                params: method.params.clone(),
                                body: ClosureBody::Untyped(b.clone()),
                                captured: env.clone(),
                                type_ctx: env.type_ctx.clone(),
                                fun_type: None,
                            })),
                        };
                        let runtime_method = runtime_method_from_decl(
                            format!("{type_name}::{}", method.name),
                            method,
                            body_callable,
                        );
                        if let Some(aspect_name) = &impl_block.aspect_name {
                            let aspect_type_args = impl_block
                                .aspect_type_args
                                .iter()
                                .map(runtime_type_key)
                                .collect();
                            runtime.register_aspect_method(
                                target_id,
                                type_name,
                                aspect_name,
                                impl_block.aspect_id,
                                aspect_type_args,
                                &method.name,
                                runtime_method,
                            );
                        } else if runtime_method.receiver.is_none() {
                            runtime.register_type_value(
                                target_id,
                                type_name,
                                &method.name,
                                runtime_method,
                            );
                        } else {
                            runtime.register_inherent_method(
                                target_id,
                                type_name,
                                &method.name,
                                runtime_method,
                            );
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // Alias registration
    for (alias, canonical) in aliases {
        if let Some(val) = env
            .get(canonical)
            .or_else(|| std_core_lookup(canonical, runtime))
        {
            if env.get(alias).is_none() {
                env.define(alias, val);
            }
        }
    }

    // Pass 2
    for decl in decls {
        if !matches!(decl, TypedDecl::Fun(_) | TypedDecl::Impl(_)) {
            eval_decl(decl, env, runtime)?;
            // ADR-0042: register a top-level let/mut's value by SymbolId too, right
            // after its initializer runs — the same moment `env.define` already binds
            // it by name. This is what lets `Call::callee_id` dispatch work for a
            // module-level first-class function value the same way it already does
            // for `fn` declarations, without changing when the binding becomes
            // available (a call before this line executes still misses, exactly as
            // it does today via `env`).
            let stamped = match decl {
                TypedDecl::Let(d) => d.def_id.map(|id| (id, d.name.as_str())),
                TypedDecl::Mut(d) => d.def_id.map(|id| (id, d.name.as_str())),
                _ => None,
            };
            if let Some((id, name)) = stamped {
                if let Some(value) = env.get(name) {
                    runtime.register_symbol_value(id, value);
                }
            }
        }
    }

    Ok(())
}

/// Locate and execute `main()` in `env`. Called after all passes complete.
fn run_main(env: &mut Environment, runtime: &RuntimeRegistry) -> Result<(), MetelError> {
    let dummy = Span {
        start: 0,
        end: 0,
        filename: "<program>".to_string(),
        line: 0,
        col: 0,
    };
    let main_body = match env.get("main") {
        Some(Value::Callable(RuntimeCallable::Closure(rc))) => rc.body.clone(),
        Some(Value::Unit) => {
            return Err(MetelError::panic(
                RuntimeErrorCode::R0002,
                "main() is generic — not supported",
                &dummy,
            ))
        }
        Some(_) => {
            return Err(MetelError::panic(
                RuntimeErrorCode::R0002,
                "`main` is not a function",
                &dummy,
            ))
        }
        None => {
            return Err(MetelError::panic(
                RuntimeErrorCode::R0001,
                "no main() function defined",
                &dummy,
            ))
        }
    };
    profiler_enter("main");
    let main_sig = match &main_body {
        ClosureBody::Typed(b) => eval_block(b, env, runtime),
        ClosureBody::Untyped(_) => {
            profiler_exit();
            return Err(MetelError::panic(
                RuntimeErrorCode::R0002,
                "main() body could not be typed",
                &dummy,
            ));
        }
    };
    profiler_exit();
    match main_sig? {
        Signal::Value(_) | Signal::Return(_) => Ok(()),
        other => Err(MetelError::internal(format!(
            "unexpected signal from main(): {other:?}"
        ))),
    }
}

// ── Block and declaration evaluation ─────────────────────────────────────────

/// Evaluate a block: push scope, run stmts, return tail (or Unit).
/// Non-Value signals (Return, Break, Continue) short-circuit and propagate out.
pub fn eval_block(
    block: &TypedBlock,
    env: &mut Environment,
    runtime: &RuntimeRegistry,
) -> Result<Signal, MetelError> {
    env.push_scope();
    for decl in &block.stmts {
        let sig = eval_decl(decl, env, runtime)?;
        match sig {
            Signal::Value(_) => {}
            other => {
                env.pop_scope();
                return Ok(other);
            }
        }
    }
    let result = match &block.tail {
        Some(tail) => eval_expr(tail, env, runtime),
        None => Ok(Signal::Value(Value::Unit)),
    };
    env.pop_scope();
    result
}

/// Evaluate a single declaration inside a block or at the top level.
fn eval_decl(
    decl: &TypedDecl,
    env: &mut Environment,
    runtime: &RuntimeRegistry,
) -> Result<Signal, MetelError> {
    match decl {
        TypedDecl::Let(d) => match eval_expr(&d.value, env, runtime)? {
            Signal::Value(val) => {
                env.define(&d.name, val);
                Ok(Signal::Value(Value::Unit))
            }
            other => Ok(other),
        },
        TypedDecl::Mut(d) => match eval_expr(&d.value, env, runtime)? {
            Signal::Value(val) => {
                env.define(&d.name, val);
                Ok(Signal::Value(Value::Unit))
            }
            other => Ok(other),
        },
        TypedDecl::Fun(f) => {
            let (body, ctx) = match &f.body {
                FunBody::Typed(b) => (ClosureBody::Typed(b.clone()), None),
                FunBody::Generic(b) => (ClosureBody::Untyped(b.clone()), env.type_ctx.clone()),
                // `native` functions are stdlib-only and top-level; they cannot
                // appear as a nested declaration.
                FunBody::Native(_) => {
                    return Err(MetelError::internal(
                        "native function in nested declaration position",
                    ))
                }
            };
            // Define a placeholder first so the closure can see itself via shared Rc
            // (enables self-recursion for functions defined inside other functions).
            env.define(&f.name, Value::Unit);
            let captured = env.clone();
            let closure = Value::Callable(RuntimeCallable::Closure(Rc::new(ClosureValue {
                name: Some(f.name.clone()),
                params: f.params.clone(),
                body,
                captured,
                type_ctx: ctx,
                fun_type: None,
            })));
            env.set(&f.name, closure);
            Ok(Signal::Value(Value::Unit))
        }
        TypedDecl::Stmt(s) => eval_stmt(s, env, runtime),
        // Type-level declarations have no runtime representation.
        TypedDecl::Struct(_) | TypedDecl::Enum(_) | TypedDecl::Impl(_) | TypedDecl::Aspect(_) => {
            Ok(Signal::Value(Value::Unit))
        }
    }
}

// ── Statement evaluation ──────────────────────────────────────────────────────

pub fn eval_stmt(
    stmt: &TypedStmt,
    env: &mut Environment,
    runtime: &RuntimeRegistry,
) -> Result<Signal, MetelError> {
    match stmt {
        TypedStmt::Expr(e) => {
            // Must propagate non-Value signals (Break/Continue/Return) that arise from
            // control-flow expressions used in statement position, e.g. `if (x) { break; }`.
            match eval_expr(e, env, runtime)? {
                Signal::Value(_) => Ok(Signal::Value(Value::Unit)),
                other => Ok(other),
            }
        }
        TypedStmt::Return(r) => {
            let val = match &r.value {
                Some(e) => eval_expr(e, env, runtime)?.into_value(),
                None => Value::Unit,
            };
            Ok(Signal::Return(val))
        }
        TypedStmt::Break(b) => {
            let val = match &b.value {
                Some(e) => eval_expr(e, env, runtime)?.into_value(),
                None => Value::Unit,
            };
            Ok(Signal::Break(val))
        }
        TypedStmt::Continue(_) => Ok(Signal::Continue),

        TypedStmt::While(w) => {
            loop {
                match eval_expr(&w.condition, env, runtime)? {
                    Signal::Value(Value::Boolean(false)) => break,
                    Signal::Value(Value::Boolean(true)) => {}
                    Signal::Value(_) => return Err(MetelError::internal(
                        "while: expected boolean condition (typechecker should have caught this)",
                    )),
                    other => return Ok(other), // propagate Return from condition
                }
                match eval_block(&w.body, env, runtime)? {
                    Signal::Value(_) | Signal::Continue => {}
                    Signal::Break(_) => break,
                    Signal::Return(v) => return Ok(Signal::Return(v)),
                }
            }
            Ok(Signal::Value(Value::Unit))
        }

        TypedStmt::For(f) => {
            // The init binding lives in its own scope so it doesn't leak out.
            // PoC note: if eval_block errors inside the loop, this scope is not
            // popped (errors are fatal so it doesn't matter in practice).
            env.push_scope();
            if let Some(init) = &f.init {
                match init {
                    TypedForInit::Let(d) => {
                        let val = eval_expr(&d.value, env, runtime)?.into_value();
                        env.define(&d.name, val);
                    }
                    TypedForInit::Mut(d) => {
                        let val = eval_expr(&d.value, env, runtime)?.into_value();
                        env.define(&d.name, val);
                    }
                    TypedForInit::Expr(e) => {
                        eval_expr(e, env, runtime)?;
                    }
                }
            }
            let result = loop {
                if let Some(cond) = &f.condition {
                    match eval_expr(cond, env, runtime)? {
                        Signal::Value(Value::Boolean(false)) => {
                            break Ok(Signal::Value(Value::Unit))
                        }
                        Signal::Value(Value::Boolean(true)) => {}
                        Signal::Value(_) => break Err(MetelError::internal(
                            "for: expected boolean condition (typechecker should have caught this)",
                        )),
                        other => break Ok(other),
                    }
                }
                match eval_block(&f.body, env, runtime)? {
                    Signal::Value(_) | Signal::Continue => {}
                    Signal::Break(_) => break Ok(Signal::Value(Value::Unit)),
                    Signal::Return(v) => break Ok(Signal::Return(v)),
                }
                if let Some(step) = &f.step {
                    eval_expr(step, env, runtime)?;
                }
            };
            env.pop_scope();
            result
        }

        TypedStmt::ForIn(fi) => {
            let iterable = eval_expr(&fi.iterable, env, runtime)?.into_value();
            eval_for_in(
                &fi.binding,
                fi.mutable,
                iterable,
                &fi.body,
                &fi.span,
                env,
                runtime,
            )
        }
    }
}

fn eval_for_in(
    binding: &str,
    _mutable: bool,
    iterable: Value,
    body: &TypedBlock,
    span: &Span,
    env: &mut Environment,
    runtime: &RuntimeRegistry,
) -> Result<Signal, MetelError> {
    // Fast path for built-in sequence types.
    let fast_items: Option<Vec<Value>> = match &iterable {
        Value::Array(rc) => Some(rc.borrow().clone()),
        Value::Struct { name, fields, .. } if name == "Range" => {
            let s = range_field(fields, "start", span)?;
            let e = range_field(fields, "end", span)?;
            Some((s..e).map(Value::I64).collect())
        }
        Value::Struct { name, fields, .. } if name == "RangeInclusive" => {
            let s = range_field(fields, "start", span)?;
            let e = range_field(fields, "end", span)?;
            Some((s..=e).map(Value::I64).collect())
        }
        _ => None,
    };

    if let Some(items) = fast_items {
        for item in items {
            env.push_scope();
            env.define(binding, item);
            let sig = eval_block(body, env, runtime)?;
            env.pop_scope();
            match sig {
                Signal::Value(_) | Signal::Continue => {}
                Signal::Break(_) => break,
                Signal::Return(v) => return Ok(Signal::Return(v)),
            }
        }
        return Ok(Signal::Value(Value::Unit));
    }

    // User-defined Iterable: dispatch through the receiver type's `next` (by id).
    let type_name = match &iterable {
        Value::Struct { name, .. } => name.clone(),
        _ => {
            return Err(MetelError::panic(
                RuntimeErrorCode::R0011,
                "for-in: expected Array, Range, or Iterable value",
                span,
            ))
        }
    };
    let next_fn = runtime
        .resolve_value_type_id(&iterable)
        .and_then(|id| runtime.get_regular_method(id, "next"))
        .ok_or_else(|| {
            MetelError::panic(
                RuntimeErrorCode::R0011,
                format!("for-in: `{type_name}` does not implement Iterable (no `next` method)"),
                span,
            )
        })?;

    let iter_cell = Rc::new(RefCell::new(deep_clone_value(iterable)));
    loop {
        let result = call::call_method_function(
            next_fn.body.clone(),
            call::ReceiverBinding::Shared(Rc::clone(&iter_cell)),
            vec![],
            span,
            runtime,
        )?
        .into_value();
        let maybe_item: Option<Value> = match result {
            Value::Enum {
                name,
                variant,
                mut fields,
                ..
            } if name == "Perhaps" => {
                if variant == "None" {
                    None
                } else {
                    Some(fields.remove("value").unwrap_or(Value::Unit))
                }
            }
            _ => {
                return Err(MetelError::internal(
                    "Iterable::next: expected Perhaps value",
                ))
            }
        };
        match maybe_item {
            None => break,
            Some(item) => {
                env.push_scope();
                env.define(binding, item);
                let sig = eval_block(body, env, runtime)?;
                env.pop_scope();
                match sig {
                    Signal::Value(_) | Signal::Continue => {}
                    Signal::Break(_) => break,
                    Signal::Return(v) => return Ok(Signal::Return(v)),
                }
            }
        }
    }
    Ok(Signal::Value(Value::Unit))
}

fn range_field(
    fields: &HashMap<String, Value>,
    name: &str,
    _span: &Span,
) -> Result<i64, MetelError> {
    match fields.get(name) {
        Some(Value::I64(n)) => Ok(*n),
        _ => Err(MetelError::internal(format!(
            "range: missing or non-Int field `{name}`"
        ))),
    }
}

// ── Expression evaluation ─────────────────────────────────────────────────────

pub fn eval_expr(
    expr: &TypedExpr,
    env: &mut Environment,
    runtime: &RuntimeRegistry,
) -> Result<Signal, MetelError> {
    match expr {
        TypedExpr::Literal(lit, ty, _) => {
            use crate::ast::{FloatKind, IntKind};
            let val = match lit {
                // Unsuffixed int/float literals are polymorphic; their resolved type
                // is determined by context (defaulting to i64/f64 when unconstrained).
                Literal::Int(n) => match ty {
                    crate::types::Type::I8 => Value::I8(*n as i8),
                    crate::types::Type::I16 => Value::I16(*n as i16),
                    crate::types::Type::I32 => Value::I32(*n as i32),
                    crate::types::Type::U8 => Value::U8(*n as u8),
                    crate::types::Type::U16 => Value::U16(*n as u16),
                    crate::types::Type::U32 => Value::U32(*n as u32),
                    crate::types::Type::U64 => Value::U64(*n as u64),
                    _ => Value::I64(*n), // i64 (default) and Int alias
                },
                Literal::Float(f) => match ty {
                    crate::types::Type::F32 => Value::F32(*f as f32),
                    _ => Value::F64(*f), // f64 (default) and Float alias
                },
                Literal::SizedInt { value, kind } => match kind {
                    IntKind::I8 => Value::I8(*value as i8),
                    IntKind::I16 => Value::I16(*value as i16),
                    IntKind::I32 => Value::I32(*value as i32),
                    IntKind::I64 => Value::I64(*value as i64),
                    IntKind::U8 => Value::U8(*value as u8),
                    IntKind::U16 => Value::U16(*value as u16),
                    IntKind::U32 => Value::U32(*value as u32),
                    IntKind::U64 => Value::U64(*value as u64),
                },
                Literal::SizedFloat { value, kind } => match kind {
                    FloatKind::F32 => Value::F32(*value as f32),
                    FloatKind::F64 => Value::F64(*value),
                },
                Literal::Char(c) => Value::Char(*c),
                Literal::Boolean(b) => Value::Boolean(*b),
                Literal::Str(s) => Value::Str(s.clone()),
                Literal::None => Value::Enum {
                    name: "Perhaps".into(),
                    type_id: Some(crate::symbols::SYM_TYPE_PERHAPS),
                    variant: "None".into(),
                    fields: HashMap::new(),
                },
                Literal::Unit => Value::Unit,
            };
            Ok(Signal::Value(val))
        }

        TypedExpr::Ident(name, _, span) => {
            match env.get(name).or_else(|| std_core_lookup(name, runtime)) {
                Some(val) => Ok(Signal::Value(val)),
                None => Err(MetelError::panic(
                    RuntimeErrorCode::R0003,
                    format!("undefined variable `{name}`"),
                    span,
                )),
            }
        }

        TypedExpr::Path(segments, _, _) => {
            // Unit enum variant: `Colour::Red` → Value::Enum { name: "Colour", variant: "Red", fields: {} }
            // A single-segment path is treated as an ident lookup.
            if segments.len() == 1 {
                let name = &segments[0];
                let span = expr.span();
                match env.get(name).or_else(|| std_core_lookup(name, runtime)) {
                    Some(val) => Ok(Signal::Value(val)),
                    None => Err(MetelError::panic(
                        RuntimeErrorCode::R0003,
                        format!("undefined variable `{name}`"),
                        span,
                    )),
                }
            } else {
                if let Some(val) = runtime
                    .resolve_path_value(segments)
                    .or_else(|| env.get(&segments.join("::")))
                {
                    return Ok(Signal::Value(val));
                }
                let name = segments[segments.len() - 2].clone();
                let variant = segments[segments.len() - 1].clone();
                Ok(Signal::Value(Value::Enum {
                    name,
                    // Unit enum variant resolved by surface path at runtime; no
                    // resolver context here, so dispatch falls back to the name.
                    type_id: None,
                    variant,
                    fields: HashMap::new(),
                }))
            }
        }

        TypedExpr::Tuple(elems, _, _) => {
            let mut vals = Vec::with_capacity(elems.len());
            for e in elems {
                vals.push(eval_expr(e, env, runtime)?.into_value());
            }
            Ok(Signal::Value(Value::Tuple(vals)))
        }

        TypedExpr::Array(elems, _, _) => {
            let mut vals = Vec::with_capacity(elems.len());
            for e in elems {
                vals.push(eval_expr(e, env, runtime)?.into_value());
            }
            Ok(Signal::Value(Value::Array(Rc::new(RefCell::new(vals)))))
        }

        TypedExpr::RepeatArray(elem, n, _, _) => {
            let val = eval_expr(elem, env, runtime)?.into_value();
            let vals = (0..*n).map(|_| val.clone()).collect::<Vec<_>>();
            Ok(Signal::Value(Value::Array(Rc::new(RefCell::new(vals)))))
        }

        TypedExpr::BinOp(lhs, op, rhs, _, span) => {
            // Short-circuit logical ops before evaluating rhs.
            if matches!(op, BinOp::And) {
                let l = eval_expr(lhs, env, runtime)?.into_value();
                return match l {
                    Value::Boolean(false) => Ok(Signal::Value(Value::Boolean(false))),
                    Value::Boolean(true) => eval_expr(rhs, env, runtime),
                    _ => Err(MetelError::internal(
                        "&&: expected boolean (typechecker should have caught this)",
                    )),
                };
            }
            if matches!(op, BinOp::Or) {
                let l = eval_expr(lhs, env, runtime)?.into_value();
                return match l {
                    Value::Boolean(true) => Ok(Signal::Value(Value::Boolean(true))),
                    Value::Boolean(false) => eval_expr(rhs, env, runtime),
                    _ => Err(MetelError::internal(
                        "||: expected boolean (typechecker should have caught this)",
                    )),
                };
            }

            let lv = eval_expr(lhs, env, runtime)?.into_value();
            let rv = eval_expr(rhs, env, runtime)?.into_value();
            lvalue::eval_binop(op, lv, rv, span)
        }

        TypedExpr::UnaryOp(op, operand, _, span) => {
            match op {
                UnaryOp::Ref => return match &**operand {
                    TypedExpr::Ident(name, _, _) => env.get_rc(name)
                        .map(|rc| Signal::Value(Value::Pointer(rc)))
                        .ok_or_else(|| MetelError::panic(RuntimeErrorCode::R0003, format!("undefined variable `{name}`"), span)),
                    other if is_lvalue_path_typed(other) => {
                        let v = eval_expr(operand, env, runtime)?.into_value();
                        Ok(Signal::Value(Value::Pointer(Rc::new(RefCell::new(v)))))
                    }
                    _ => Err(MetelError::internal("address-of requires an addressable lvalue (identifier, field access, tuple access, or array index)")),
                },
                UnaryOp::RefMut => return match &**operand {
                    TypedExpr::Ident(name, _, _) => env.get_rc(name)
                        .map(|rc| Signal::Value(Value::MutPointer(rc)))
                        .ok_or_else(|| MetelError::panic(RuntimeErrorCode::R0003, format!("undefined variable `{name}`"), span)),
                    other if is_lvalue_path_typed(other) => {
                        let (root_name, path) = build_mut_path(other, env, runtime, span)?;
                        let root = env.get_rc(&root_name).ok_or_else(|| MetelError::panic(
                            RuntimeErrorCode::R0003, format!("undefined variable `{root_name}`"), span))?;
                        Ok(Signal::Value(Value::MutFieldPointer { root, path }))
                    }
                    _ => Err(MetelError::internal("mutable address-of requires an addressable lvalue")),
                },
                _ => {}
            }
            let v = eval_expr(operand, env, runtime)?.into_value();
            let result =
                match (op, v) {
                    (UnaryOp::Neg, Value::I64(n)) => Value::I64(n.wrapping_neg()),
                    (UnaryOp::Neg, Value::I8(n)) => Value::I8(n.wrapping_neg()),
                    (UnaryOp::Neg, Value::I16(n)) => Value::I16(n.wrapping_neg()),
                    (UnaryOp::Neg, Value::I32(n)) => Value::I32(n.wrapping_neg()),
                    (UnaryOp::Neg, Value::F64(f)) => Value::F64(-f),
                    (UnaryOp::Neg, Value::F32(f)) => Value::F32(-f),
                    (UnaryOp::Not, Value::Boolean(b)) => Value::Boolean(!b),
                    (UnaryOp::Deref, Value::Pointer(rc))
                    | (UnaryOp::Deref, Value::MutPointer(rc)) => rc.borrow().clone(),
                    (UnaryOp::Deref, Value::MutFieldPointer { root, path }) => {
                        read_path(&root.borrow(), &path, span)?
                    }
                    (UnaryOp::Neg, _) => return Err(MetelError::internal(
                        "unary `-`: expected numeric type (typechecker should have caught this)",
                    )),
                    (UnaryOp::Not, _) => {
                        return Err(MetelError::internal(
                            "unary `!`: expected boolean (typechecker should have caught this)",
                        ))
                    }
                    (UnaryOp::Deref, _) => {
                        return Err(MetelError::internal(
                            "unary `*`: expected pointer (typechecker should have caught this)",
                        ))
                    }
                    _ => unreachable!("Ref/RefMut handled above"),
                };
            Ok(Signal::Value(result))
        }

        TypedExpr::Cast {
            expr: inner,
            target_type,
            span,
            ..
        } => {
            let v = eval_expr(inner, env, runtime)?.into_value();
            // Dispatch through From impl using the full aspect-signature key
            // "Target::From<Source>::from", then fall back to "Target::from"
            // (used by built-in Int::from / Float::from which have no type arg).
            if let crate::ast::TypeExpr::Named(target_name, _) = target_type {
                let from_fn = runtime_type_name(&v)
                    .and_then(|source| runtime.get_from_method(target_name, source));
                if let Some(f) = from_fn {
                    return call::call_function(Value::Callable(f.body), vec![v], span, runtime);
                }
            }
            // Identity cast fallback (same type, no from registered).
            Ok(Signal::Value(v))
        }

        TypedExpr::TupleAccess {
            object,
            index,
            span,
            ..
        } => {
            let v = eval_expr(object, env, runtime)?.into_value();
            match v {
                Value::Tuple(elems) => {
                    elems
                        .into_iter()
                        .nth(*index)
                        .map(Signal::Value)
                        .ok_or_else(|| {
                            MetelError::panic(
                                RuntimeErrorCode::R0005,
                                format!("tuple index {index} out of bounds"),
                                span,
                            )
                        })
                }
                _ => Err(MetelError::internal(
                    "tuple access on non-tuple (typechecker should have caught this)",
                )),
            }
        }

        TypedExpr::Index {
            object,
            index,
            span,
            ..
        } => {
            let arr = eval_expr(object, env, runtime)?.into_value();
            let idx = eval_expr(index, env, runtime)?.into_value();
            let i: usize = match idx {
                Value::U64(u) => u as usize,
                _ => {
                    return Err(MetelError::internal(
                        "index: expected u64 index (typechecker should have caught this)",
                    ))
                }
            };
            match arr {
                Value::Array(rc) => {
                    let borrowed = rc.borrow();
                    if i >= borrowed.len() {
                        Err(MetelError::panic(
                            RuntimeErrorCode::R0004,
                            format!("index {i} out of bounds (len {})", borrowed.len()),
                            span,
                        ))
                    } else {
                        Ok(Signal::Value(borrowed[i].clone()))
                    }
                }
                _ => Err(MetelError::internal(
                    "index: expected Array (typechecker should have caught this)",
                )),
            }
        }

        TypedExpr::If {
            condition,
            then_branch,
            else_branch,
            ..
        } => {
            match eval_expr(condition, env, runtime)? {
                Signal::Value(Value::Boolean(true)) => eval_block(then_branch, env, runtime),
                Signal::Value(Value::Boolean(false)) => match else_branch {
                    Some(branch) => eval_block(branch, env, runtime),
                    None => Ok(Signal::Value(Value::Unit)),
                },
                Signal::Value(_) => Err(MetelError::internal(
                    "if: expected boolean condition (typechecker should have caught this)",
                )),
                other => Ok(other), // propagate Return from condition
            }
        }

        TypedExpr::Loop { body, .. } => loop {
            match eval_block(body, env, runtime)? {
                Signal::Value(_) | Signal::Continue => {}
                Signal::Break(val) => return Ok(Signal::Value(val)),
                Signal::Return(v) => return Ok(Signal::Return(v)),
            }
        },

        TypedExpr::Match(m) => {
            let scrutinee = eval_expr(&m.scrutinee, env, runtime)?.into_value();
            for arm in &m.arms {
                let mut bindings = HashMap::new();
                if !pattern::match_pattern(&arm.pattern, &scrutinee, &mut bindings) {
                    continue;
                }
                // Evaluate the guard (if any) in a scope that includes pattern bindings.
                if let Some(guard) = &arm.guard {
                    env.push_scope();
                    for (k, v) in &bindings {
                        env.define(k, v.clone());
                    }
                    let guard_val = eval_expr(guard, env, runtime)?.into_value();
                    env.pop_scope();
                    match guard_val {
                        Value::Boolean(true) => {}
                        Value::Boolean(false) => continue,
                        _ => return Err(MetelError::internal(
                            "match guard: expected boolean (typechecker should have caught this)",
                        )),
                    }
                }
                // Execute the arm body in a scope with pattern bindings.
                env.push_scope();
                for (k, v) in bindings {
                    env.define(&k, v);
                }
                let result = eval_block(&arm.body, env, runtime);
                env.pop_scope();
                return result;
            }
            Err(MetelError::panic(
                RuntimeErrorCode::R0006,
                "match: no arm matched scrutinee",
                &m.span,
            ))
        }

        TypedExpr::Assign {
            target,
            op,
            value,
            span,
            ..
        } => {
            use crate::ast::AssignOp;
            use crate::typed_ast::TypedPlace;
            let rhs = eval_expr(value, env, runtime)?.into_value();
            match target {
                TypedPlace::Ident(name, ident_span) => {
                    let new_val = if matches!(op, AssignOp::Assign) {
                        rhs
                    } else {
                        let cur = env.get(name).ok_or_else(|| {
                            MetelError::panic(
                                RuntimeErrorCode::R0003,
                                format!("assign: undefined `{name}`"),
                                ident_span,
                            )
                        })?;
                        lvalue::apply_assign_op(op, cur, rhs, span)?
                    };
                    if !env.set(name, new_val) {
                        return Err(MetelError::panic(
                            RuntimeErrorCode::R0003,
                            format!("assign: undefined `{name}`"),
                            ident_span,
                        ));
                    }
                    Ok(Signal::Value(Value::Unit))
                }

                TypedPlace::Deref {
                    object,
                    span: tspan,
                } => {
                    let ptr = eval_expr(object, env, runtime)?.into_value();
                    match ptr {
                        Value::Pointer(rc) | Value::MutPointer(rc) => {
                            let new_val = if matches!(op, AssignOp::Assign) {
                                rhs
                            } else {
                                let cur = rc.borrow().clone();
                                lvalue::apply_assign_op(op, cur, rhs, span)?
                            };
                            *rc.borrow_mut() = new_val;
                        }
                        Value::MutFieldPointer { root, path } => {
                            let new_val = if matches!(op, AssignOp::Assign) {
                                rhs
                            } else {
                                let cur = read_path(&root.borrow(), &path, tspan)?;
                                lvalue::apply_assign_op(op, cur, rhs, span)?
                            };
                            write_path(&mut root.borrow_mut(), &path, new_val, tspan)?;
                        }
                        _ => {
                            return Err(MetelError::panic(
                                RuntimeErrorCode::R0003,
                                "assign: dereference target is not a pointer",
                                tspan,
                            ))
                        }
                    }
                    Ok(Signal::Value(Value::Unit))
                }

                TypedPlace::Index {
                    object,
                    index,
                    span: _tspan,
                } => {
                    let arr_val = lvalue::eval_typed_place_value(object, env, runtime)?;
                    let idx_val = eval_expr(index, env, runtime)?.into_value();
                    let i =
                        match idx_val {
                            Value::U64(u) => u as usize,
                            _ => return Err(MetelError::internal(
                                "index: expected u64 index (typechecker should have caught this)",
                            )),
                        };
                    match arr_val {
                        Value::Array(rc) => {
                            let len = rc.borrow().len();
                            if i >= len {
                                return Err(MetelError::panic(
                                    RuntimeErrorCode::R0004, format!("index {i} out of bounds (len {len})"), span,
                                ));
                            }
                            let new_val = if matches!(op, AssignOp::Assign) {
                                rhs
                            } else {
                                let cur = rc.borrow()[i].clone();
                                lvalue::apply_assign_op(op, cur, rhs, span)?
                            };
                            rc.borrow_mut()[i] = new_val;
                            Ok(Signal::Value(Value::Unit))
                        }
                        _ => Err(MetelError::internal(
                            "index assign: receiver is not an Array (typechecker should have caught this)",
                        )),
                    }
                }

                TypedPlace::Field {
                    object,
                    field,
                    span: tspan,
                } => {
                    let (rc, path) =
                        lvalue::resolve_field_assign_root(object, field, env, runtime, tspan)?;
                    let mut borrowed = rc.borrow_mut();
                    // Navigate intermediate path segments to reach the parent struct.
                    let mut cur: &mut Value = &mut borrowed;
                    for segment in &path[..path.len() - 1] {
                        cur = match cur {
                            Value::Struct { fields, .. } | Value::Enum { fields, .. } => {
                                fields.get_mut(*segment).ok_or_else(|| {
                                    MetelError::panic(
                                        RuntimeErrorCode::R0008,
                                        format!("field assign: no field `{segment}`"),
                                        tspan,
                                    )
                                })?
                            }
                            _ => {
                                return Err(MetelError::internal(format!(
                                    "field assign: `{segment}` is not a struct/enum"
                                )))
                            }
                        };
                    }
                    let fields = match cur {
                        Value::Struct { fields, .. } | Value::Enum { fields, .. } => fields,
                        _ => return Err(MetelError::internal(
                            "field assign: receiver is not a struct/enum (typechecker should have caught this)",
                        )),
                    };
                    let leaf = path.last().expect("path is non-empty");
                    let new_val = if matches!(op, AssignOp::Assign) {
                        rhs
                    } else {
                        let cur = fields.get(*leaf).cloned().ok_or_else(|| {
                            MetelError::panic(
                                RuntimeErrorCode::R0008,
                                format!("field assign: no field `{leaf}`"),
                                tspan,
                            )
                        })?;
                        lvalue::apply_assign_op(op, cur, rhs, span)?
                    };
                    fields.insert((*leaf).to_string(), new_val);
                    Ok(Signal::Value(Value::Unit))
                }
            }
        }

        TypedExpr::StructLiteral {
            path,
            fields,
            type_id,
            span: _,
            ..
        } => {
            let mut field_vals: HashMap<String, Value> = HashMap::new();
            for (name, expr) in fields {
                field_vals.insert(name.clone(), eval_expr(expr, env, runtime)?.into_value());
            }
            if path.len() == 2 {
                Ok(Signal::Value(Value::Enum {
                    name: path[0].clone(),
                    type_id: *type_id,
                    variant: path[1].clone(),
                    fields: field_vals,
                }))
            } else {
                let name = path
                    .last()
                    .ok_or_else(|| MetelError::internal("struct literal: empty path"))?
                    .clone();
                Ok(Signal::Value(Value::Struct {
                    name,
                    type_id: *type_id,
                    fields: field_vals,
                }))
            }
        }

        TypedExpr::FieldAccess {
            object,
            field,
            span,
            ..
        } => {
            let mut val = eval_expr(object, env, runtime)?.into_value();
            if let Some(deref) = deref_value(&val, span)? {
                val = deref;
            }
            let fields = match &val {
                Value::Struct { fields, .. } | Value::Enum { fields, .. } => fields,
                _ => {
                    return Err(MetelError::internal(
                        "field access on non-struct/enum (typechecker should have caught this)",
                    ))
                }
            };
            fields
                .get(field)
                .cloned()
                .map(Signal::Value)
                .ok_or_else(|| {
                    MetelError::panic(
                        RuntimeErrorCode::R0008,
                        format!("no field `{field}` on value"),
                        span,
                    )
                })
        }

        TypedExpr::MethodCall {
            receiver,
            method,
            args,
            dispatch,
            span,
            ..
        } => {
            let recv_val = eval_expr(receiver, env, runtime)?.into_value();
            let arg_vals: Vec<Value> = args
                .iter()
                .map(|a| eval_expr(a, env, runtime).map(Signal::into_value))
                .collect::<Result<_, _>>()?;

            // Runtime methods are dispatched through the runtime registry, not lexical env.
            let recv_type_view = deref_value(&recv_val, span)?.unwrap_or_else(|| recv_val.clone());
            let method_entry = match dispatch {
                // Elaboration resolved this as an aspect call: resolve the receiver to its
                // type SymbolId and dispatch the aspect method by id, falling back to the
                // general value lookup (inherent + pattern methods).
                MethodDispatch::Aspect { aspect_id } => runtime
                    .resolve_value_type_id(&recv_type_view)
                    .and_then(|tid| runtime.get_aspect_method_by_id(tid, *aspect_id, method))
                    .or_else(|| runtime.get_method_for_value(&recv_type_view, method)),
                // Inherent or unresolved: use the full lookup (inherent is tried first).
                MethodDispatch::Inherent | MethodDispatch::Dynamic => {
                    runtime.get_method_for_value(&recv_type_view, method)
                }
            }
            .ok_or_else(|| {
                MetelError::panic(
                    RuntimeErrorCode::R0009,
                    format!("method `{method}` not found on this value"),
                    span,
                )
            })?;
            let func = method_entry.body.clone();
            match method_entry.receiver {
                Some(crate::ast::ReceiverKind::Ref | crate::ast::ReceiverKind::RefMut) => {
                    // For field-access chains (e.g. `pair.a.tick()`) we can't hand the
                    // evaluator a direct Rc to the field because fields are stored by value
                    // inside the parent struct's HashMap.  Instead we clone the leaf value
                    // into a fresh cell, call through it, then write the (possibly mutated)
                    // value back into the parent struct.
                    let mut field_writeback: Option<FieldWriteback> = None;

                    let receiver_binding = match receiver.as_ref() {
                        TypedExpr::Ident(name, _, _) => {
                            match env.get_rc(name).map(|cell| {
                                let inner = match &*cell.borrow() {
                                    Value::Pointer(inner) | Value::MutPointer(inner) => {
                                        Some(Rc::clone(inner))
                                    }
                                    _ => None,
                                };
                                inner.unwrap_or(cell)
                            }) {
                                Some(cell) => call::ReceiverBinding::Shared(cell),
                                None => call::ReceiverBinding::Value(recv_type_view.clone()),
                            }
                        }
                        TypedExpr::FieldAccess { .. } => match lvalue_field_cell(receiver, env) {
                            Some((struct_cell, path, leaf_cell)) => {
                                let binding = call::ReceiverBinding::Shared(Rc::clone(&leaf_cell));
                                field_writeback = Some((struct_cell, path, leaf_cell));
                                binding
                            }
                            None => receiver_cell_from_value(&recv_val)
                                .map(call::ReceiverBinding::Shared)
                                .unwrap_or(call::ReceiverBinding::Value(recv_type_view.clone())),
                        },
                        _ => receiver_cell_from_value(&recv_val)
                            .map(call::ReceiverBinding::Shared)
                            .unwrap_or(call::ReceiverBinding::Value(recv_type_view.clone())),
                    };

                    let result = call::call_method_function(
                        func,
                        receiver_binding,
                        arg_vals,
                        span,
                        runtime,
                    )?;

                    if let Some((struct_cell, path, leaf_cell)) = field_writeback {
                        let new_val = leaf_cell.borrow().clone();
                        let last = path.last().unwrap();
                        let prefix = &path[..path.len() - 1];
                        let mut borrow = struct_cell.borrow_mut();
                        let mut cur: &mut Value = &mut borrow;
                        for seg in prefix {
                            match cur {
                                Value::Struct { fields, .. } | Value::Enum { fields, .. } => {
                                    cur = fields.get_mut(seg.as_str()).unwrap();
                                }
                                _ => break,
                            }
                        }
                        if let Value::Struct { fields, .. } | Value::Enum { fields, .. } = cur {
                            fields.insert(last.clone(), new_val);
                        }
                    }

                    Ok(result)
                }
                Some(crate::ast::ReceiverKind::Value) => {
                    // By-value `self`: bind the receiver as the first parameter.
                    // Routed through call_method_function (not call_function) so a
                    // generic method body's scheme is resolved from the registry's
                    // method env, not the flat scheme env.
                    call::call_method_function(
                        func,
                        call::ReceiverBinding::Value(recv_type_view),
                        arg_vals,
                        span,
                        runtime,
                    )
                }
                None => Err(MetelError::panic(
                    RuntimeErrorCode::R0009,
                    format!("runtime method `{method}` is not callable with a receiver"),
                    span,
                )),
            }
        }

        TypedExpr::Call {
            callee,
            args,
            callee_id,
            span,
            ..
        } => {
            // A statically-resolved callee carries its SymbolId (METEL-180 overloads
            // and METEL-187 ordinary top-level functions); dispatch through the symbol
            // registry instead of evaluating the callee expression by name.
            let func_val = match callee_id {
                Some(id) => match runtime.get_symbol_value(*id).cloned() {
                    Some(value) => value,
                    // An overload id with no registered value is an internal error —
                    // all overloads are registered in Pass 1a, before any user code runs.
                    None if id.0 >= crate::symbols::OVERLOAD_SYM_START => {
                        return Err(MetelError::internal(format!(
                            "no runtime value registered for overload symbol {id:?}"
                        )));
                    }
                    // A top-level `let`/`mut`'s id is registered lazily, when its own
                    // Pass 2 initializer runs (ADR-0042) — a miss here can legitimately
                    // mean "called before that line executed," the same "used before
                    // defined" error `eval_expr` already produces via plain name lookup
                    // (R0003), not a bug. Evaluating by name here reuses that existing,
                    // correct error path rather than duplicating it.
                    None if runtime.is_let_mut_def_id(*id) => {
                        eval_expr(callee, env, runtime)?.into_value()
                    }
                    // Any other top-level callable id (ordinary `fn`s, registered in
                    // Pass 1b before any user code runs) missing its value is a genuine
                    // resolver/construction bug, not a legitimate runtime state.
                    None => {
                        return Err(MetelError::internal(format!(
                            "no runtime value registered for callable symbol {id:?}"
                        )));
                    }
                },
                None => eval_expr(callee, env, runtime)?.into_value(),
            };
            let arg_vals: Vec<Value> = args
                .iter()
                .map(|a| eval_expr(a, env, runtime).map(Signal::into_value))
                .collect::<Result<_, _>>()?;
            call::call_function(func_val, arg_vals, span, runtime)
        }

        TypedExpr::Closure {
            params, body, ty, ..
        } => {
            let captured = env.capture_clone();
            Ok(Signal::Value(Value::Callable(RuntimeCallable::Closure(
                Rc::new(ClosureValue {
                    name: None,
                    params: params.clone(),
                    body: ClosureBody::Typed(body.clone()),
                    captured,
                    type_ctx: None,
                    fun_type: Some(ty.clone()),
                }),
            ))))
        }

        TypedExpr::GenericClosure {
            name, params, body, ..
        } => {
            let captured = env.capture_clone();
            Ok(Signal::Value(Value::Callable(RuntimeCallable::Closure(
                Rc::new(ClosureValue {
                    name: name.clone(),
                    params: params.clone(),
                    body: ClosureBody::Untyped(body.clone()),
                    captured,
                    type_ctx: env.type_ctx.clone(),
                    fun_type: None,
                }),
            ))))
        }
    }
}
