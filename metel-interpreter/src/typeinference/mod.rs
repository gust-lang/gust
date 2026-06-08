//! Type inference module for Metel.
//!
//! Implements Hindley-Milner type inference with let-polymorphism.
//! See `docs/internal/typechecker.md` for theory background and implementation notes.

use crate::ast::{AspectMethod, ReceiverKind, Span, Visibility};
use crate::error::MetelError;
use crate::types::Type;
use std::collections::{HashMap, HashSet};
use std::time::Instant;

// ── Phase 1: Type Variables ───────────────────────────────────────────────────

/// A type variable representing an unknown type during inference.
/// Each type variable has a unique ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TypeVar(pub u32);

impl std::fmt::Display for TypeVar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "?t{}", self.0)
    }
}

/// Counter for generating fresh type variables.
///
/// # Invariant: TypeVar identity is global
///
/// `TypeVar` equality means identity — two vars with the same `u32` are the *same* variable.
/// All `TypeVarGenerator` instances within a single type-check run must therefore be
/// coordinated: each new generator must start past the highest counter value produced by
/// any earlier generator.  Creating an independent `TypeVarGenerator::new()` in a call site
/// that produces vars intended to be globally unique will cause collisions — the "fresh"
/// var may be identical to an already-used one, producing self-referential substitutions
/// and infinite recursion in `Substitution::apply`.
///
/// The correct pattern: `InferContext` owns the generator for Pass 1.  After Pass 1,
/// call `ctx.split_gen()` to obtain a new generator that starts past all Pass 1 vars,
/// then thread that single instance through Pass 2 (and any intermediate steps like
/// `register_builtin_poly_schemes`).
pub struct TypeVarGenerator {
    counter: u32,
}

impl TypeVarGenerator {
    /// Create a new type variable generator.
    pub fn new() -> Self {
        TypeVarGenerator { counter: 0 }
    }

    pub fn with_counter(start: u32) -> Self {
        TypeVarGenerator { counter: start }
    }

    /// Generate a fresh type variable.
    pub fn fresh(&mut self) -> TypeVar {
        let var = TypeVar(self.counter);
        self.counter += 1;
        var
    }

    /// Get the current counter state (for testing).
    pub fn counter(&self) -> u32 {
        self.counter
    }
}

impl Default for TypeVarGenerator {
    fn default() -> Self {
        Self::new()
    }
}

// ── Phase 2: Inference Types ──────────────────────────────────────────────────

/// A type that may contain unresolved type variables.
/// Used during inference before all types are known.
/// Distinct from `Type`, which is fully resolved and contains no variables.
#[derive(Debug, Clone, PartialEq)]
pub enum InferType {
    /// A fully resolved concrete type.
    Concrete(Type),
    /// An unknown type represented by a type variable.
    Var(TypeVar),
    /// The bottom type `!` — produced by diverging expressions (infinite loops with
    /// no reachable `break`, `return`, `panic!`). Unifies with any type.
    Never,
    /// A function type with parameter types and a return type.
    Fun(Vec<InferType>, Box<InferType>),
    /// A tuple type.
    Tuple(Vec<InferType>),
    /// A homogeneous array type.
    Array(Box<InferType>),
    /// A fixed-size array type `[T; N]`.
    SizedArray(Box<InferType>, u64),
    /// A shared pointer type.
    Pointer(Box<InferType>),
    /// A mutable pointer type.
    MutPointer(Box<InferType>),
    /// A named type (struct, enum) with type arguments.
    Named(String, Vec<InferType>),
}

impl InferType {
    pub fn int() -> Self {
        InferType::Concrete(Type::I64)
    }
    pub fn float() -> Self {
        InferType::Concrete(Type::F64)
    }
    pub fn bool() -> Self {
        InferType::Concrete(Type::Boolean)
    }
    pub fn str() -> Self {
        InferType::Concrete(Type::Str)
    }
    pub fn unit() -> Self {
        InferType::Concrete(Type::Unit)
    }
    pub fn never() -> Self {
        InferType::Never
    }
    #[allow(dead_code)] // public API used by typeinference test suite
    pub fn var(v: TypeVar) -> Self {
        InferType::Var(v)
    }
}

impl std::fmt::Display for InferType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InferType::Concrete(t) => write!(f, "{}", t),
            InferType::Var(v) => write!(f, "{}", v),
            InferType::Never => write!(f, "!"),
            InferType::Fun(params, ret) => {
                write!(f, "(")?;
                for (i, p) in params.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", p)?;
                }
                write!(f, ") -> {}", ret)
            }
            InferType::Tuple(ts) => {
                write!(f, "(")?;
                for (i, t) in ts.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", t)?;
                }
                write!(f, ")")
            }
            InferType::Array(t) => write!(f, "{}[]", t),
            InferType::SizedArray(t, n) => write!(f, "[{}; {}]", t, n),
            InferType::Pointer(t) => write!(f, "*{}", t),
            InferType::MutPointer(t) => write!(f, "*mut {}", t),
            InferType::Named(name, args) => {
                write!(f, "{}", name)?;
                if !args.is_empty() {
                    write!(f, "<")?;
                    for (i, a) in args.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{}", a)?;
                    }
                    write!(f, ">")?;
                }
                Ok(())
            }
        }
    }
}

// ── Phase 3: Substitution ─────────────────────────────────────────────────────

/// A map from type variables to their resolved `InferType`s.
/// The right-hand side may still contain variables — `apply` chases them transitively.
#[derive(Debug, Clone, Default)]
pub struct Substitution {
    bindings: HashMap<TypeVar, InferType>,
}

impl Substitution {
    pub fn new() -> Self {
        Substitution {
            bindings: HashMap::new(),
        }
    }

    /// Record that `var` maps to `ty`.
    pub fn bind(&mut self, var: TypeVar, ty: InferType) {
        self.bindings.insert(var, ty);
    }

    /// Look up the direct binding for `var`, if any.
    pub fn lookup(&self, var: TypeVar) -> Option<&InferType> {
        self.bindings.get(&var)
    }

    /// Recursively replace all type variables in `ty` using this substitution.
    pub fn apply(&self, ty: &InferType) -> InferType {
        match ty {
            InferType::Concrete(_) | InferType::Never => ty.clone(),
            InferType::Var(v) => match self.bindings.get(v) {
                Some(resolved) => self.apply(resolved),
                None => ty.clone(),
            },
            InferType::Fun(params, ret) => InferType::Fun(
                params.iter().map(|p| self.apply(p)).collect(),
                Box::new(self.apply(ret)),
            ),
            InferType::Tuple(ts) => InferType::Tuple(ts.iter().map(|t| self.apply(t)).collect()),
            InferType::Array(t) => InferType::Array(Box::new(self.apply(t))),
            InferType::SizedArray(t, n) => InferType::SizedArray(Box::new(self.apply(t)), *n),
            InferType::Pointer(t) => InferType::Pointer(Box::new(self.apply(t))),
            InferType::MutPointer(t) => InferType::MutPointer(Box::new(self.apply(t))),
            InferType::Named(name, args) => {
                InferType::Named(name.clone(), args.iter().map(|a| self.apply(a)).collect())
            }
        }
    }

    /// Produce a substitution equivalent to applying `self` first, then `other`
    /// (i.e. `other ∘ self` in mathematical notation).
    ///
    /// `self` wins on overlap: if both substitutions bind `?t0`, `other` is applied
    /// to `self`'s value — not to the variable itself — so a concrete value from
    /// `self` passes through `other` unchanged. This matches Algorithm W, where a
    /// variable is unified at most once and later substitutions refine free variables
    /// in the *values*, not the *keys*.
    pub fn compose(&self, other: &Substitution) -> Substitution {
        let mut result = Substitution::new();
        for (var, ty) in &self.bindings {
            result.bind(*var, other.apply(ty));
        }
        for (var, ty) in &other.bindings {
            result.bindings.entry(*var).or_insert_with(|| ty.clone());
        }
        result
    }
}

// ── Phase 4: Unification ──────────────────────────────────────────────────────

/// Returns true if `var` appears anywhere inside `ty`.
/// Used by the occurs check to prevent infinite types like `?t0 = Array<?t0>`.
fn occurs_in(var: TypeVar, ty: &InferType) -> bool {
    match ty {
        InferType::Concrete(_) | InferType::Never => false,
        InferType::Var(v) => *v == var,
        InferType::Fun(params, ret) => {
            params.iter().any(|p| occurs_in(var, p)) || occurs_in(var, ret)
        }
        InferType::Tuple(ts) => ts.iter().any(|t| occurs_in(var, t)),
        InferType::Array(t) => occurs_in(var, t),
        InferType::SizedArray(t, _) => occurs_in(var, t),
        InferType::Pointer(t) | InferType::MutPointer(t) => occurs_in(var, t),
        InferType::Named(_, args) => args.iter().any(|a| occurs_in(var, a)),
    }
}

/// Bind `var` to `ty`, failing if the occurs check would create an infinite type.
fn bind_var(var: TypeVar, ty: &InferType) -> Result<Substitution, MetelError> {
    if let InferType::Var(v) = ty {
        if *v == var {
            return Ok(Substitution::new());
        }
    }
    if occurs_in(var, ty) {
        return Err(MetelError::internal(format!(
            "occurs check failed: {} occurs in {}",
            var, ty
        )));
    }
    let mut s = Substitution::new();
    s.bind(var, ty.clone());
    Ok(s)
}

/// Unify two inference types, returning a substitution that makes them equal.
///
/// Returns an error if the types are structurally incompatible or if the occurs
/// check detects an infinite type.
pub fn unify(a: &InferType, b: &InferType) -> Result<Substitution, MetelError> {
    match (a, b) {
        // Never is the bottom type — it coerces to any type.
        (InferType::Never, _) | (_, InferType::Never) => Ok(Substitution::new()),
        (InferType::Concrete(t1), InferType::Concrete(t2)) => {
            if t1 == t2 {
                Ok(Substitution::new())
            } else {
                Err(MetelError::internal(format!(
                    "cannot unify {} with {}",
                    a, b
                )))
            }
        }
        (InferType::Var(v), _) => bind_var(*v, b),
        (_, InferType::Var(v)) => bind_var(*v, a),
        (InferType::Fun(params1, ret1), InferType::Fun(params2, ret2)) => {
            if params1.len() != params2.len() {
                return Err(MetelError::internal(format!(
                    "cannot unify {} with {}",
                    a, b
                )));
            }
            let mut subst = Substitution::new();
            for (p1, p2) in params1.iter().zip(params2.iter()) {
                let s = unify(&subst.apply(p1), &subst.apply(p2))?;
                subst = subst.compose(&s);
            }
            let s = unify(&subst.apply(ret1), &subst.apply(ret2))?;
            Ok(subst.compose(&s))
        }
        (InferType::Tuple(ts1), InferType::Tuple(ts2)) => {
            if ts1.len() != ts2.len() {
                return Err(MetelError::internal(format!(
                    "cannot unify {} with {}",
                    a, b
                )));
            }
            let mut subst = Substitution::new();
            for (t1, t2) in ts1.iter().zip(ts2.iter()) {
                let s = unify(&subst.apply(t1), &subst.apply(t2))?;
                subst = subst.compose(&s);
            }
            Ok(subst)
        }
        (InferType::Array(t1), InferType::Array(t2)) => unify(t1, t2),
        (InferType::SizedArray(t1, n1), InferType::SizedArray(t2, n2)) => {
            if n1 != n2 {
                return Err(MetelError::internal(format!(
                    "cannot unify {} with {}",
                    a, b
                )));
            }
            unify(t1, t2)
        }
        // [T; N] coerces to T[] (one-directional)
        (InferType::SizedArray(t1, _), InferType::Array(t2))
        | (InferType::Array(t1), InferType::SizedArray(t2, _)) => unify(t1, t2),
        (InferType::Pointer(t1), InferType::Pointer(t2))
        | (InferType::MutPointer(t1), InferType::MutPointer(t2))
        | (InferType::Pointer(t1), InferType::MutPointer(t2))
        | (InferType::MutPointer(t1), InferType::Pointer(t2)) => unify(t1, t2),
        (InferType::Named(n1, args1), InferType::Named(n2, args2)) => {
            if n1 != n2 || args1.len() != args2.len() {
                return Err(MetelError::internal(format!(
                    "cannot unify {} with {}",
                    a, b
                )));
            }
            let mut subst = Substitution::new();
            for (a1, a2) in args1.iter().zip(args2.iter()) {
                let s = unify(&subst.apply(a1), &subst.apply(a2))?;
                subst = subst.compose(&s);
            }
            Ok(subst)
        }
        _ => Err(MetelError::internal(format!(
            "cannot unify {} with {}",
            a, b
        ))),
    }
}

// ── Phase 5: Constraints ──────────────────────────────────────────────────────

/// A deferred type equation: `lhs` and `rhs` must unify, recorded with the
/// source `span` so that failures produce actionable error messages.
#[derive(Debug, Clone)]
pub struct Constraint {
    pub lhs: InferType,
    pub rhs: InferType,
    pub span: Span,
}

impl Constraint {
    pub fn new(lhs: InferType, rhs: InferType, span: Span) -> Self {
        Self { lhs, rhs, span }
    }
}

fn is_integer_type(t: &Type) -> bool {
    matches!(
        t,
        Type::I8 | Type::I16 | Type::I32 | Type::I64 | Type::U8 | Type::U16 | Type::U32 | Type::U64
    )
}

fn is_float_type(t: &Type) -> bool {
    matches!(t, Type::F32 | Type::F64)
}

/// Solve a list of constraints by unifying each `lhs`/`rhs` pair in order.
///
/// The running substitution is applied to both sides before each unification
/// so that earlier bindings propagate into later constraints. Errors are
/// reported with the source span of the offending constraint.
///
/// Integer/float literal TypeVars are validated: if one resolves to a concrete
/// non-numeric type, T0001 is raised at the constraint site.
#[allow(dead_code)] // kept as a standalone solver helper for tests and profiling comparisons
pub fn solve_constraints(
    constraints: Vec<Constraint>,
    integer_literal_vars: &HashSet<TypeVar>,
    float_literal_vars: &HashSet<TypeVar>,
) -> Result<Substitution, MetelError> {
    let mut subst = Substitution::new();
    for c in constraints {
        apply_constraint(&mut subst, &c, integer_literal_vars, float_literal_vars)?;
    }
    Ok(subst)
}

fn apply_constraint(
    subst: &mut Substitution,
    constraint: &Constraint,
    integer_literal_vars: &HashSet<TypeVar>,
    float_literal_vars: &HashSet<TypeVar>,
) -> Result<(), MetelError> {
    let lhs = subst.apply(&constraint.lhs);
    let rhs = subst.apply(&constraint.rhs);
    let solved = unify(&lhs, &rhs).map_err(|_| {
        MetelError::type_error(
            crate::error::TypeErrorCode::T0001,
            format!("cannot unify {} with {}", lhs, rhs),
            &constraint.span,
        )
    })?;
    *subst = subst.compose(&solved);
    validate_literal_bindings(
        subst,
        integer_literal_vars,
        float_literal_vars,
        &constraint.span,
    )
}

fn validate_literal_bindings(
    subst: &Substitution,
    integer_literal_vars: &HashSet<TypeVar>,
    float_literal_vars: &HashSet<TypeVar>,
    span: &Span,
) -> Result<(), MetelError> {
    for &var in integer_literal_vars {
        match subst.apply(&InferType::Var(var)) {
            InferType::Var(_) | InferType::Never => {}
            InferType::Concrete(t) if is_integer_type(&t) => {}
            other => {
                return Err(MetelError::type_error(
                    crate::error::TypeErrorCode::T0001,
                    format!("cannot unify integer literal with `{other}`"),
                    span,
                ))
            }
        }
    }
    for &var in float_literal_vars {
        match subst.apply(&InferType::Var(var)) {
            InferType::Var(_) | InferType::Never => {}
            InferType::Concrete(t) if is_float_type(&t) => {}
            other => {
                return Err(MetelError::type_error(
                    crate::error::TypeErrorCode::T0001,
                    format!("cannot unify float literal with `{other}`"),
                    span,
                ))
            }
        }
    }
    Ok(())
}

// ── Phase 6: Type Schemes ─────────────────────────────────────────────────────

/// Collect all type variables that appear free in `ty`.
pub fn free_vars(ty: &InferType) -> HashSet<TypeVar> {
    match ty {
        InferType::Concrete(_) | InferType::Never => HashSet::new(),
        InferType::Var(v) => [*v].into(),
        InferType::Fun(params, ret) => {
            let mut vars = free_vars(ret);
            for p in params {
                vars.extend(free_vars(p));
            }
            vars
        }
        InferType::Tuple(ts) => ts.iter().flat_map(free_vars).collect(),
        InferType::Array(t) => free_vars(t),
        InferType::SizedArray(t, _) => free_vars(t),
        InferType::Pointer(t) | InferType::MutPointer(t) => free_vars(t),
        InferType::Named(_, args) => args.iter().flat_map(free_vars).collect(),
    }
}

/// A universally quantified type: `∀ quantified_vars. ty`.
///
/// Variables in `quantified_vars` are locally owned — each use site gets
/// fresh copies via `instantiate`, enabling let-polymorphism.
///
/// `param_names` optionally holds the source-level names of each quantified variable,
/// in the same sorted order as `quantified_vars`. Used during construction-at-call-time
/// to resolve type annotations like `T[]` inside generic function bodies.
#[derive(Debug, Clone, PartialEq)]
pub struct TypeScheme {
    pub quantified_vars: Vec<TypeVar>,
    /// Source-level names for quantified vars (same order). Empty for builtins.
    pub param_names: Vec<String>,
    pub ty: InferType,
}

impl TypeScheme {
    /// A monomorphic scheme — no quantified variables.
    pub fn mono(ty: InferType) -> Self {
        Self {
            quantified_vars: vec![],
            param_names: vec![],
            ty,
        }
    }
}

impl std::fmt::Display for TypeScheme {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.quantified_vars.is_empty() {
            write!(f, "{}", self.ty)
        } else {
            write!(f, "∀")?;
            for (i, v) in self.quantified_vars.iter().enumerate() {
                if i > 0 {
                    write!(f, ", ")?;
                }
                write!(f, "{}", v)?;
            }
            write!(f, ". {}", self.ty)
        }
    }
}

/// Generalize `ty` into a type scheme by quantifying over all type variables
/// that appear free in `ty` but not in `env_free_vars`.
///
/// `env_free_vars` is the set of variables that are still being solved in the
/// surrounding environment — those must not be captured.
pub fn generalize(ty: InferType, env_free_vars: &HashSet<TypeVar>) -> TypeScheme {
    let mut quantified: Vec<TypeVar> = free_vars(&ty).difference(env_free_vars).copied().collect();
    quantified.sort();
    TypeScheme {
        quantified_vars: quantified,
        param_names: vec![],
        ty,
    }
}

/// Like `generalize` but also records the source-level name for each quantified variable.
/// `name_map` maps TypeVar ID → param name (e.g. `{5 → "T"}`).
pub fn generalize_with_names(
    ty: InferType,
    env_free_vars: &HashSet<TypeVar>,
    name_map: &HashMap<TypeVar, String>,
) -> TypeScheme {
    let mut scheme = generalize(ty, env_free_vars);
    scheme.param_names = scheme
        .quantified_vars
        .iter()
        .map(|v| name_map.get(v).cloned().unwrap_or_default())
        .collect();
    scheme
}

/// Instantiate a type scheme by replacing each quantified variable with a
/// fresh type variable from `gen`. Called once per use site.
pub fn instantiate(scheme: &TypeScheme, gen: &mut TypeVarGenerator) -> InferType {
    let mut subst = Substitution::new();
    for &var in &scheme.quantified_vars {
        subst.bind(var, InferType::Var(gen.fresh()));
    }
    subst.apply(&scheme.ty)
}

/// Like `instantiate` but also returns the mapping from each original quantified
/// TypeVar to the fresh TypeVar it was replaced with.
pub fn instantiate_with_renaming(
    scheme: &TypeScheme,
    gen: &mut TypeVarGenerator,
) -> (InferType, HashMap<TypeVar, TypeVar>) {
    let mut renaming = HashMap::new();
    let mut subst = Substitution::new();
    for &var in &scheme.quantified_vars {
        let fresh = gen.fresh();
        subst.bind(var, InferType::Var(fresh));
        renaming.insert(var, fresh);
    }
    (subst.apply(&scheme.ty), renaming)
}

// ── Enum environment ─────────────────────────────────────────────────────────

/// A single field entry in a struct or enum variant, carrying its declaration metadata.
#[derive(Debug, Clone)]
pub struct FieldEntry {
    pub name: String,
    pub ty: InferType,
    pub span: Span,
    pub visibility: Visibility,
}

#[derive(Debug, Clone)]
pub struct VariantInfo {
    pub name: String,
    pub fields: Vec<FieldEntry>,
}

#[derive(Debug, Clone)]
pub struct EnumInfo {
    pub type_params: Vec<TypeVar>,
    pub variants: Vec<VariantInfo>,
}

// ── Type Definition Registry ──────────────────────────────────────────────────

/// Unified store of all named type definitions across all pipeline phases.
/// Created by `build_registry` and injected into `InferContext` before inference begins.
///
/// Owns the canonical description of every struct, enum, aspect, and impl in the
/// program. Both the inference pass (Pass 1) and the construction pass (Pass 2)
/// derive their type information from this registry instead of maintaining parallel
/// copies. Fields and variant payloads carry their declaration `Span` so that
/// downstream errors can point back to the source location.
///
/// ## Elaboration interface
///
/// The elaboration pass (`elaborator::elaborate`) uses two methods on this registry:
///
/// - `aspect_declaring_module(name)` — returns the module path that declared `name` as an
///   aspect; used to look up the aspect's `SymbolId` in the name-resolver's symbol table.
///
/// That SymbolId is the key stored in `TypedImplBlock::aspect_id` and in
/// `RuntimeAspectImpl::aspect_id`.  The elaboration pass has no other dependency on this
/// registry; it does not read or write inference-phase state.
#[derive(Debug, Clone)]
pub struct TypeDefinitionRegistry {
    /// struct name → fields with declaration spans.
    struct_env: HashMap<String, Vec<FieldEntry>>,
    /// struct name → declaring module path.
    struct_decl_modules: HashMap<String, Vec<String>>,
    /// Ordered type-parameter TypeVars per generic struct (absent for non-generic structs).
    struct_type_params: HashMap<String, Vec<TypeVar>>,
    /// Ordered type-parameter names per generic struct/enum. Parallel to struct_type_params.
    /// Used when setting up impl method scopes so param names resolve to TypeVars.
    struct_generic_names: HashMap<String, Vec<String>>,
    /// Polymorphic method schemes for methods on generic structs that reference the struct's
    /// type params. Key: (type_name, method_name) → (scheme, struct_tvars_ordered).
    /// struct_tvars_ordered[i] corresponds to the i-th type arg of the receiver at the call site.
    method_scheme_env: HashMap<String, HashMap<String, (TypeScheme, Vec<TypeVar>)>>,
    /// Per-type-param aspect bounds for generic structs and enums.
    /// Key: type name. Value: one Vec<String> per type param (same order as struct_type_params),
    /// each containing the aspect names that param must satisfy.
    type_param_bounds: HashMap<String, Vec<Vec<String>>>,
    /// Aspect bounds per generic function. Key: function name.
    /// Value: map from each quantified TypeVar to the list of required aspect names.
    fun_bounds: HashMap<String, HashMap<TypeVar, Vec<String>>>,
    /// Tracks which struct names were registered in each lexical scope so they
    /// can be removed on scope exit. Empty when outside any scoped block.
    struct_scope_stack: Vec<Vec<String>>,
    method_env: HashMap<String, HashMap<String, InferType>>,
    method_receiver_env: HashMap<String, HashMap<String, ReceiverKind>>,
    enum_env: HashMap<String, EnumInfo>,
    /// enum name → declaring module path.
    enum_decl_modules: HashMap<String, Vec<String>>,
    /// aspect name → ordered list of method names the aspect declares.
    /// Used to verify impl blocks are complete.
    aspect_env: HashMap<String, Vec<String>>,
    /// aspect name → declaring module path.
    aspect_decl_modules: HashMap<String, Vec<String>>,
    /// aspect name → full declared methods, including default bodies.
    aspect_method_defs: HashMap<String, Vec<AspectMethod>>,
    /// (target_type_name, aspect_name) → list of type-arg vectors, one per registered impl.
    /// E.g. ("Int", "From") → [[Type::F64]] means `impl From<Float> for Int`.
    impl_aspect_env: HashMap<(String, String), Vec<Vec<Type>>>,
}

impl TypeDefinitionRegistry {
    pub fn new() -> Self {
        Self {
            struct_env: HashMap::new(),
            struct_decl_modules: HashMap::new(),
            struct_type_params: HashMap::new(),
            struct_generic_names: HashMap::new(),
            method_scheme_env: HashMap::new(),
            type_param_bounds: HashMap::new(),
            fun_bounds: HashMap::new(),
            struct_scope_stack: Vec::new(),
            method_env: HashMap::new(),
            method_receiver_env: HashMap::new(),
            enum_env: HashMap::new(),
            enum_decl_modules: HashMap::new(),
            aspect_env: HashMap::new(),
            aspect_decl_modules: HashMap::new(),
            aspect_method_defs: HashMap::new(),
            impl_aspect_env: HashMap::new(),
        }
    }

    pub fn register_struct_fields(
        &mut self,
        name: String,
        fields: Vec<FieldEntry>,
        declaring_module: Vec<String>,
    ) {
        self.struct_env.insert(name.clone(), fields);
        self.struct_decl_modules
            .insert(name.clone(), declaring_module);
        if let Some(scope) = self.struct_scope_stack.last_mut() {
            scope.push(name);
        }
    }

    pub fn push_struct_scope(&mut self) {
        self.struct_scope_stack.push(Vec::new());
    }

    pub fn pop_struct_scope(&mut self) {
        if let Some(names) = self.struct_scope_stack.pop() {
            for name in names {
                self.struct_env.remove(&name);
                self.struct_decl_modules.remove(&name);
            }
        }
    }

    pub fn register_method(&mut self, type_name: String, method_name: String, fun_ty: InferType) {
        self.method_env
            .entry(type_name)
            .or_default()
            .insert(method_name, fun_ty);
    }

    pub fn register_method_receiver(
        &mut self,
        type_name: String,
        method_name: String,
        receiver_kind: ReceiverKind,
    ) {
        self.method_receiver_env
            .entry(type_name)
            .or_default()
            .insert(method_name, receiver_kind);
    }

    pub fn register_struct_type_params(&mut self, name: String, type_params: Vec<TypeVar>) {
        self.struct_type_params.insert(name, type_params);
    }

    pub fn register_struct_generic_names(&mut self, name: String, param_names: Vec<String>) {
        self.struct_generic_names.insert(name, param_names);
    }

    pub fn struct_generic_names_for(&self, name: &str) -> Option<&Vec<String>> {
        self.struct_generic_names.get(name)
    }

    pub fn register_method_scheme(
        &mut self,
        type_name: String,
        method_name: String,
        scheme: TypeScheme,
        struct_tvars: Vec<TypeVar>,
    ) {
        self.method_scheme_env
            .entry(type_name)
            .or_default()
            .insert(method_name, (scheme, struct_tvars));
    }

    pub fn method_scheme_for(
        &self,
        type_name: &str,
        method_name: &str,
    ) -> Option<&(TypeScheme, Vec<TypeVar>)> {
        self.method_scheme_env.get(type_name)?.get(method_name)
    }

    pub fn register_type_param_bounds(&mut self, name: String, bounds: Vec<Vec<String>>) {
        self.type_param_bounds.insert(name, bounds);
    }

    pub fn type_param_bounds_for(&self, name: &str) -> Option<&Vec<Vec<String>>> {
        self.type_param_bounds.get(name)
    }

    /// Returns true if `type_name` has a registered `impl AspectName` in the env.
    pub fn impl_aspect_env_has(&self, type_name: &str, aspect_name: &str) -> bool {
        self.impl_aspect_env
            .contains_key(&(type_name.to_string(), aspect_name.to_string()))
    }

    pub fn register_fun_bounds(&mut self, name: String, bounds: HashMap<TypeVar, Vec<String>>) {
        if !bounds.is_empty() {
            self.fun_bounds.insert(name, bounds);
        }
    }

    pub fn fun_bounds_for(&self, name: &str) -> Option<&HashMap<TypeVar, Vec<String>>> {
        self.fun_bounds.get(name)
    }

    pub fn register_enum(&mut self, name: String, info: EnumInfo, declaring_module: Vec<String>) {
        self.enum_env.insert(name.clone(), info);
        self.enum_decl_modules.insert(name, declaring_module);
    }

    pub fn struct_fields(&self, name: &str) -> Option<&Vec<FieldEntry>> {
        self.struct_env.get(name)
    }

    pub fn struct_type_params_for(&self, name: &str) -> Option<&Vec<TypeVar>> {
        self.struct_type_params.get(name)
    }

    pub fn method_type(&self, type_name: &str, method_name: &str) -> Option<&InferType> {
        self.method_env.get(type_name)?.get(method_name)
    }

    pub fn method_receiver_kind(
        &self,
        type_name: &str,
        method_name: &str,
    ) -> Option<&ReceiverKind> {
        self.method_receiver_env.get(type_name)?.get(method_name)
    }

    pub fn enum_info(&self, name: &str) -> Option<&EnumInfo> {
        self.enum_env.get(name)
    }

    pub fn struct_declaring_module(&self, name: &str) -> Option<&Vec<String>> {
        self.struct_decl_modules.get(name)
    }

    pub fn enum_declaring_module(&self, name: &str) -> Option<&Vec<String>> {
        self.enum_decl_modules.get(name)
    }

    pub fn register_aspect(&mut self, name: String, methods: Vec<String>) {
        self.aspect_env.insert(name, methods);
    }

    /// Record that aspect `name` was declared in `module`. Called once per `AspectDecl`
    /// during registry construction and once per builtin aspect in
    /// `typechecker::registry::register_primitive_type_bindings`.
    pub fn register_aspect_declaring_module(&mut self, name: String, module: Vec<String>) {
        self.aspect_decl_modules.insert(name, module);
    }

    /// Return the module path that declared aspect `name`.
    ///
    /// Used by the **elaboration pass** to look up the aspect's `SymbolId` in the
    /// name-resolver symbol table — the only link between the string-keyed registry and the
    /// stable `SymbolId` world.
    pub fn aspect_declaring_module(&self, name: &str) -> Option<&Vec<String>> {
        self.aspect_decl_modules.get(name)
    }

    pub fn register_aspect_method_defs(&mut self, name: String, methods: Vec<AspectMethod>) {
        self.aspect_method_defs.insert(name, methods);
    }

    pub fn aspect_method_defs(&self, name: &str) -> Option<&Vec<AspectMethod>> {
        self.aspect_method_defs.get(name)
    }

    pub fn register_aspect_impl(&mut self, target: String, aspect: String, type_args: Vec<Type>) {
        self.impl_aspect_env
            .entry((target, aspect))
            .or_default()
            .push(type_args);
    }

    /// Checks `(target, "From")` for an impl with first type-arg matching `source`.
    pub fn has_from_impl(&self, target: &str, source: &Type) -> bool {
        self.impl_aspect_env
            .get(&(target.to_string(), "From".to_string()))
            .map(|impls| impls.iter().any(|args| args.first() == Some(source)))
            .unwrap_or(false)
    }

    /// Returns the element type registered for `(target, "Iterable")`, if any.
    pub fn iterable_elem_type(&self, target: &str) -> Option<&Type> {
        self.impl_aspect_env
            .get(&(target.to_string(), "Iterable".to_string()))
            .and_then(|impls| impls.first())
            .and_then(|args| args.first())
    }

    pub(crate) fn raw_struct_env(&self) -> &HashMap<String, Vec<FieldEntry>> {
        &self.struct_env
    }

    pub(crate) fn raw_struct_type_params(&self) -> &HashMap<String, Vec<TypeVar>> {
        &self.struct_type_params
    }

    pub(crate) fn raw_enum_env(&self) -> &HashMap<String, EnumInfo> {
        &self.enum_env
    }

    pub(crate) fn raw_method_env(&self) -> &HashMap<String, HashMap<String, InferType>> {
        &self.method_env
    }

    /// Copy all entries from `other` into `self`, without overwriting existing entries.
    /// Used by `check_impl` to seed a module's registry with type definitions from
    /// already-checked dependency modules. See ADR-0032.
    pub fn merge_from(&mut self, other: &TypeDefinitionRegistry) {
        for (k, v) in &other.struct_env {
            self.struct_env
                .entry(k.clone())
                .or_insert_with(|| v.clone());
        }
        for (k, v) in &other.struct_decl_modules {
            self.struct_decl_modules
                .entry(k.clone())
                .or_insert_with(|| v.clone());
        }
        for (k, v) in &other.struct_type_params {
            self.struct_type_params
                .entry(k.clone())
                .or_insert_with(|| v.clone());
        }
        for (k, v) in &other.struct_generic_names {
            self.struct_generic_names
                .entry(k.clone())
                .or_insert_with(|| v.clone());
        }
        for (k, v) in &other.method_scheme_env {
            self.method_scheme_env
                .entry(k.clone())
                .or_insert_with(|| v.clone());
        }
        for (k, v) in &other.type_param_bounds {
            self.type_param_bounds
                .entry(k.clone())
                .or_insert_with(|| v.clone());
        }
        for (k, v) in &other.fun_bounds {
            self.fun_bounds
                .entry(k.clone())
                .or_insert_with(|| v.clone());
        }
        for (k, v) in &other.method_env {
            self.method_env
                .entry(k.clone())
                .or_insert_with(|| v.clone());
        }
        for (k, v) in &other.method_receiver_env {
            self.method_receiver_env
                .entry(k.clone())
                .or_insert_with(|| v.clone());
        }
        for (k, v) in &other.enum_env {
            self.enum_env.entry(k.clone()).or_insert_with(|| v.clone());
        }
        for (k, v) in &other.enum_decl_modules {
            self.enum_decl_modules
                .entry(k.clone())
                .or_insert_with(|| v.clone());
        }
        for (k, v) in &other.aspect_env {
            self.aspect_env
                .entry(k.clone())
                .or_insert_with(|| v.clone());
        }
        for (k, v) in &other.aspect_decl_modules {
            self.aspect_decl_modules
                .entry(k.clone())
                .or_insert_with(|| v.clone());
        }
        for (k, v) in &other.aspect_method_defs {
            self.aspect_method_defs
                .entry(k.clone())
                .or_insert_with(|| v.clone());
        }
        for (k, v) in &other.impl_aspect_env {
            self.impl_aspect_env
                .entry(k.clone())
                .or_insert_with(|| v.clone());
        }
    }
}

impl Default for TypeDefinitionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ── Phase 7: Inference Context ────────────────────────────────────────────────

/// State threaded through the entire AST walk during type inference.
///
/// Owns the variable generator, both environments, and the accumulated
/// constraint list. Call `solve()` after the walk to get the final substitution.
///
/// `mono_env` is a scope stack: call `push_scope`/`pop_scope` in matched pairs
/// when entering and leaving lexical scopes (function bodies, blocks).
/// `poly_env` is scoped like `mono_env`; each `push_scope`/`pop_scope` adds/removes a layer.
pub struct InferContext {
    var_gen: TypeVarGenerator,
    mono_env: Vec<HashMap<String, (InferType, bool)>>,
    poly_env: Vec<HashMap<String, TypeScheme>>,
    constraints: Vec<Constraint>,
    current_return_type: Option<InferType>,
    current_break_type: Option<InferType>,
    registry: TypeDefinitionRegistry,
    /// Type-param name → TypeVar for the currently-being-inferred generic function.
    /// Empty when inferring a non-generic function or at top level.
    current_type_params: HashMap<String, TypeVar>,
    /// TypeVar → aspect names for the current generic function's bounded type params.
    /// Parallel to current_type_params; swapped in/out alongside it.
    current_type_param_bounds: HashMap<TypeVar, Vec<String>>,
    current_module_path: Vec<String>,
    /// Names that have same-tier glob conflicts deferred until use. (METEL-98)
    /// Maps name → list of source module paths that both export it.
    deferred_glob_conflicts: HashMap<String, Vec<Vec<String>>>,
    /// TypeVars introduced by unsuffixed integer literals (`42`, `1_000`).
    /// Any such var that is still free after constraint solving defaults to `i64`.
    integer_literal_vars: HashSet<TypeVar>,
    /// TypeVars introduced by unsuffixed float literals (`3.14`, `2.0`).
    /// Any such var that is still free after constraint solving defaults to `f64`.
    float_literal_vars: HashSet<TypeVar>,
    cached_subst: Substitution,
    solved_constraint_count: usize,
    solve_stats: SolveStats,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SolveStats {
    pub solve_calls: u64,
    pub constraints_processed: u64,
    pub solve_ns: u64,
}

impl InferContext {
    /// Create a new inference context with a pre-built registry, a generator
    /// that has already been advanced past all TypeVars allocated during registry
    /// construction (ensuring global TypeVar uniqueness), and the set of imported
    /// schemes to seed into the poly_env. See ADR-0022.
    pub fn new(
        registry: TypeDefinitionRegistry,
        gen: TypeVarGenerator,
        imported_schemes: &HashMap<String, TypeScheme>,
        current_module_path: Vec<String>,
    ) -> Self {
        let mut ctx = Self {
            var_gen: gen,
            mono_env: vec![HashMap::new()], // root scope pre-pushed
            poly_env: vec![HashMap::new()], // root scope pre-pushed
            constraints: Vec::new(),
            current_return_type: None,
            current_break_type: None,
            registry,
            current_type_params: HashMap::new(),
            current_type_param_bounds: HashMap::new(),
            current_module_path,
            deferred_glob_conflicts: HashMap::new(),
            integer_literal_vars: HashSet::new(),
            float_literal_vars: HashSet::new(),
            cached_subst: Substitution::new(),
            solved_constraint_count: 0,
            solve_stats: SolveStats::default(),
        };
        for (name, scheme) in imported_schemes {
            ctx.bind_poly(name, scheme.clone());
        }
        ctx
    }

    /// Register deferred same-tier glob conflicts. T0011 fires at the use site.
    pub fn seed_glob_conflicts(&mut self, conflicts: HashMap<String, Vec<Vec<String>>>) {
        self.deferred_glob_conflicts = conflicts;
    }

    /// If `name` has a deferred same-tier glob conflict, return T0011.
    /// Call this at every name use site before `lookup`.
    pub fn check_glob_conflict(&self, name: &str, span: &Span) -> Option<crate::error::MetelError> {
        self.deferred_glob_conflicts.get(name).map(|sources| {
            let m0 = sources.first().map(|m| m.join("::")).unwrap_or_default();
            let m1 = sources.get(1).map(|m| m.join("::")).unwrap_or_default();
            crate::error::MetelError::type_error(
                crate::error::TypeErrorCode::T0011,
                format!(
                    "import conflict: `{name}` is exported by both `{m0}` and `{m1}`; \
                     use an explicit import to disambiguate: \
                     `import {m0}::{name}` or `import {m1}::{name}`"
                ),
                span,
            )
        })
    }

    pub fn register_struct_fields(
        &mut self,
        name: String,
        fields: Vec<crate::typeinference::FieldEntry>,
    ) {
        self.registry
            .register_struct_fields(name, fields, self.current_module_path.clone());
    }

    pub fn get_struct_type_params(&self, name: &str) -> Option<&Vec<TypeVar>> {
        self.registry.struct_type_params_for(name)
    }

    pub fn push_struct_scope(&mut self) {
        self.registry.push_struct_scope();
    }
    pub fn pop_struct_scope(&mut self) {
        self.registry.pop_struct_scope();
    }

    pub fn register_method(&mut self, type_name: String, method_name: String, fun_ty: InferType) {
        self.registry
            .register_method(type_name, method_name, fun_ty);
    }

    pub fn get_struct_fields(&self, name: &str) -> Option<&Vec<crate::typeinference::FieldEntry>> {
        self.registry.struct_fields(name)
    }

    pub fn get_method_type(&self, type_name: &str, method_name: &str) -> Option<&InferType> {
        self.registry.method_type(type_name, method_name)
    }

    pub fn get_method_receiver_kind(
        &self,
        type_name: &str,
        method_name: &str,
    ) -> Option<&ReceiverKind> {
        self.registry.method_receiver_kind(type_name, method_name)
    }

    pub fn register_enum(&mut self, name: String, info: EnumInfo) {
        self.registry
            .register_enum(name, info, self.current_module_path.clone());
    }

    pub fn get_enum(&self, name: &str) -> Option<&EnumInfo> {
        self.registry.enum_info(name)
    }

    pub fn aspect_method_defs(&self, name: &str) -> Option<&Vec<AspectMethod>> {
        self.registry.aspect_method_defs(name)
    }

    pub fn has_from_impl(&self, target: &str, source: &Type) -> bool {
        self.registry.has_from_impl(target, source)
    }

    pub fn iterable_elem_type(&self, target: &str) -> Option<&Type> {
        self.registry.iterable_elem_type(target)
    }

    pub fn registry(&self) -> &TypeDefinitionRegistry {
        &self.registry
    }

    pub fn current_module_path(&self) -> &[String] {
        &self.current_module_path
    }

    pub fn registry_mut(&mut self) -> &mut TypeDefinitionRegistry {
        &mut self.registry
    }

    /// Consume the context and return its registry. Used by `check_graph` to extract
    /// accumulated type definitions after a module is checked. See ADR-0032.
    pub fn into_registry(self) -> TypeDefinitionRegistry {
        self.registry
    }

    pub fn fresh_type_var_raw(&mut self) -> TypeVar {
        self.var_gen.fresh()
    }

    /// Install a new type-param map for the duration of a generic function body.
    /// Returns the previous map so it can be restored with a second call.
    pub fn swap_type_params(&mut self, map: HashMap<String, TypeVar>) -> HashMap<String, TypeVar> {
        std::mem::replace(&mut self.current_type_params, map)
    }

    pub fn swap_type_param_bounds(
        &mut self,
        bounds: HashMap<TypeVar, Vec<String>>,
    ) -> HashMap<TypeVar, Vec<String>> {
        std::mem::replace(&mut self.current_type_param_bounds, bounds)
    }

    pub fn type_params(&self) -> &HashMap<String, TypeVar> {
        &self.current_type_params
    }

    /// Returns the aspect names required by a type param TypeVar in the current function scope.
    pub fn bounds_for_type_var(&self, tv: TypeVar) -> Option<&Vec<String>> {
        self.current_type_param_bounds.get(&tv)
    }

    /// Returns the aspect method defs from the registry.
    pub fn get_aspect_method_defs(&self, aspect: &str) -> Option<&Vec<crate::ast::AspectMethod>> {
        self.registry.aspect_method_defs(aspect)
    }

    pub fn register_fun_bounds(&mut self, name: String, bounds: HashMap<TypeVar, Vec<String>>) {
        self.registry.register_fun_bounds(name, bounds);
    }

    pub fn struct_generic_names_for(&self, name: &str) -> Option<&Vec<String>> {
        self.registry.struct_generic_names_for(name)
    }

    pub fn get_type_param_bounds(&self, name: &str) -> Option<&Vec<Vec<String>>> {
        self.registry.type_param_bounds_for(name)
    }

    pub fn register_method_scheme(
        &mut self,
        type_name: String,
        method_name: String,
        scheme: TypeScheme,
        struct_tvars: Vec<TypeVar>,
    ) {
        self.registry
            .register_method_scheme(type_name, method_name, scheme, struct_tvars);
    }

    pub fn method_scheme_for(
        &self,
        type_name: &str,
        method_name: &str,
    ) -> Option<&(TypeScheme, Vec<TypeVar>)> {
        self.registry.method_scheme_for(type_name, method_name)
    }

    /// Return a new generator whose counter starts immediately past all vars
    /// allocated by this context.  Use this to hand off to a subsequent phase
    /// (Pass 2, `register_builtin_poly_schemes`) so that every `TypeVar` ever
    /// produced during a type-check run is globally unique.
    pub fn split_gen(&self) -> TypeVarGenerator {
        TypeVarGenerator::with_counter(self.var_gen.counter())
    }

    /// Enter a new lexical scope (e.g. a function body or block).
    /// Must be matched with a call to `pop_scope`.
    pub fn push_scope(&mut self) {
        self.mono_env.push(HashMap::new());
        self.poly_env.push(HashMap::new());
    }

    /// Exit the current lexical scope, discarding all bindings introduced in it.
    /// Panics if called with no inner scope (i.e. at the root).
    pub fn pop_scope(&mut self) {
        assert!(self.mono_env.len() > 1, "pop_scope called at root scope");
        self.mono_env.pop();
        assert!(self.poly_env.len() > 1, "pop_scope called at root scope");
        self.poly_env.pop();
    }

    /// Generate a fresh type variable.
    pub fn fresh_var(&mut self) -> InferType {
        InferType::Var(self.var_gen.fresh())
    }

    /// Create a fresh TypeVar for an unsuffixed integer literal.
    /// If still free after constraint solving, it defaults to `i64`.
    pub fn fresh_integer_literal_var(&mut self) -> InferType {
        let ty = self.fresh_var();
        if let InferType::Var(tv) = ty {
            self.integer_literal_vars.insert(tv);
            InferType::Var(tv)
        } else {
            ty
        }
    }

    /// Create a fresh TypeVar for an unsuffixed float literal.
    /// If still free after constraint solving, it defaults to `f64`.
    pub fn fresh_float_literal_var(&mut self) -> InferType {
        let ty = self.fresh_var();
        if let InferType::Var(tv) = ty {
            self.float_literal_vars.insert(tv);
            InferType::Var(tv)
        } else {
            ty
        }
    }

    /// Extend `subst` so that any literal TypeVar still free (unbound to a concrete type)
    /// is defaulted: integer literal vars → `i64`, float literal vars → `f64`.
    /// Also propagates defaults through TypeVar chains: if a literal var resolves to
    /// another free TypeVar, both are bound to the default type.
    /// Call this immediately after each `ctx.solve()` before using the substitution.
    pub fn default_literal_vars(&self, subst: &Substitution) -> Substitution {
        let mut extended = subst.clone();
        for &var in &self.integer_literal_vars {
            if let InferType::Var(final_var) = extended.apply(&InferType::Var(var)) {
                extended.bind(final_var, InferType::int());
                extended.bind(var, InferType::int());
            }
        }
        for &var in &self.float_literal_vars {
            if let InferType::Var(final_var) = extended.apply(&InferType::Var(var)) {
                extended.bind(final_var, InferType::float());
                extended.bind(var, InferType::float());
            }
        }
        extended
    }

    pub fn is_integer_literal_var(&self, tv: TypeVar) -> bool {
        self.integer_literal_vars.contains(&tv)
    }

    pub fn is_float_literal_var(&self, tv: TypeVar) -> bool {
        self.float_literal_vars.contains(&tv)
    }

    /// Bind a name to a monomorphic type in the current scope.
    /// `is_mutable` is `true` for `mut` bindings, `false` for `let` bindings and parameters.
    pub fn bind_mono(&mut self, name: impl Into<String>, ty: InferType, is_mutable: bool) {
        self.mono_env
            .last_mut()
            .unwrap()
            .insert(name.into(), (ty, is_mutable));
    }

    /// Bind a name to a polymorphic type scheme in the current scope.
    pub fn bind_poly(&mut self, name: impl Into<String>, scheme: TypeScheme) {
        self.poly_env
            .last_mut()
            .unwrap()
            .insert(name.into(), scheme);
    }

    /// Bind a polymorphic scheme only if the current scope does not already
    /// contain that name. Used for lower-priority prelude names.
    pub fn bind_poly_if_absent(&mut self, name: impl Into<String>, scheme: TypeScheme) {
        self.poly_env
            .last_mut()
            .unwrap()
            .entry(name.into())
            .or_insert(scheme);
    }

    /// Look up a name. Polymorphic bindings are automatically instantiated with
    /// fresh variables; monomorphic bindings are searched innermost-scope-first.
    /// Poly env takes precedence over mono env within each scope level.
    pub fn lookup(&mut self, name: &str) -> Option<InferType> {
        if let Some(scheme) = self
            .poly_env
            .iter()
            .rev()
            .find_map(|s| s.get(name))
            .cloned()
        {
            Some(instantiate(&scheme, &mut self.var_gen))
        } else {
            self.mono_env
                .iter()
                .rev()
                .find_map(|scope| scope.get(name))
                .map(|(ty, _)| ty.clone())
        }
    }

    /// Look up a name for writing (assignment). Returns the binding's type on success.
    /// Errors:
    ///   - E0003 if the name is not in scope
    ///   - E0006 if the binding is immutable (`let` or parameter)
    pub fn lookup_for_write(&self, name: &str, span: &Span) -> Result<InferType, MetelError> {
        match self.mono_env.iter().rev().find_map(|scope| scope.get(name)) {
            None => Err(MetelError::type_error(
                crate::error::TypeErrorCode::T0003,
                format!("use of undeclared variable `{name}`"),
                span,
            )),
            Some((_, false)) => Err(MetelError::type_error(
                crate::error::TypeErrorCode::T0006,
                format!("cannot assign to immutable binding `{name}`"),
                span,
            )),
            Some((ty, true)) => Ok(ty.clone()),
        }
    }

    /// Collect all type variables that appear free across all current mono scopes.
    /// Pass this to `generalize()` to avoid capturing variables still being solved.
    pub fn env_free_vars(&self) -> HashSet<TypeVar> {
        self.mono_env
            .iter()
            .flat_map(|scope| scope.values())
            .flat_map(|(ty, _)| free_vars(ty))
            .collect()
    }

    /// Record that `lhs` and `rhs` must unify, tagged with its source location.
    pub fn add_constraint(&mut self, lhs: InferType, rhs: InferType, span: Span) {
        self.constraints.push(Constraint::new(lhs, rhs, span));
    }

    /// Solve all accumulated constraints and return the resulting substitution.
    pub fn solve(&mut self) -> Result<Substitution, MetelError> {
        let started = Instant::now();
        self.solve_stats.solve_calls += 1;

        let mut subst = self.cached_subst.clone();
        for constraint in &self.constraints[self.solved_constraint_count..] {
            apply_constraint(
                &mut subst,
                constraint,
                &self.integer_literal_vars,
                &self.float_literal_vars,
            )?;
        }

        self.solve_stats.constraints_processed +=
            (self.constraints.len() - self.solved_constraint_count) as u64;
        self.solve_stats.solve_ns += started.elapsed().as_nanos().min(u64::MAX as u128) as u64;
        self.solved_constraint_count = self.constraints.len();
        self.cached_subst = subst.clone();
        Ok(subst)
    }

    pub fn solve_stats(&self) -> SolveStats {
        self.solve_stats
    }

    /// Set the expected return type for the current function, returning the previous value.
    /// Call `pop_return_type` with the returned value to restore on function exit.
    pub fn push_return_type(&mut self, ty: InferType) -> Option<InferType> {
        self.current_return_type.replace(ty)
    }

    /// Restore the return type context after leaving a function body.
    pub fn pop_return_type(&mut self, prev: Option<InferType>) {
        self.current_return_type = prev;
    }

    /// The expected return type of the innermost enclosing function, if any.
    pub fn current_return_type(&self) -> Option<&InferType> {
        self.current_return_type.as_ref()
    }

    pub fn push_break_type(&mut self, ty: InferType) -> Option<InferType> {
        self.current_break_type.replace(ty)
    }

    pub fn pop_break_type(&mut self, prev: Option<InferType>) {
        self.current_break_type = prev;
    }

    pub fn current_break_type(&self) -> Option<&InferType> {
        self.current_break_type.as_ref()
    }
}

impl Default for InferContext {
    fn default() -> Self {
        Self::new(
            TypeDefinitionRegistry::new(),
            TypeVarGenerator::new(),
            &HashMap::new(),
            vec![],
        )
    }
}

// ── TypeCtx ───────────────────────────────────────────────────────────────────

/// Type context carried by generic closures to support construction-at-call-time.
///
/// When a generic function body (`FunBody::Generic`) is stored as `ClosureBody::Untyped`,
/// this context provides the data the typechecker's construction pass needs to produce
/// a `TypedBlock` at the point of the call, given concrete argument types.
#[derive(Debug, Clone)]
pub struct TypeCtx {
    /// Full scheme environment of the module where the closure was defined.
    pub scheme_env: HashMap<String, TypeScheme>,
    /// Accumulated type-definition registry (structs, enums, aspects, methods) visible
    /// from the module where the closure was defined.
    pub registry: TypeDefinitionRegistry,
}
