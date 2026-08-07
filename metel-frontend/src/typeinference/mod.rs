//! Type inference module for Metel.
//!
//! Implements Hindley-Milner type inference with let-polymorphism.
//! See `docs/internal/typechecker.md` for theory background and implementation notes.

use crate::ast::{AspectMethod, AssocTypeDecl, ReceiverKind, RowBound, Span, TypeExpr, Visibility};
use crate::error::MetelError;
use crate::name_resolver::{GlobTier, ModuleScope};
use crate::symbols::SymbolId;
use crate::types::Type;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::rc::Rc;
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
/// # Invariant: `TypeVar` identity is global
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
    #[must_use]
    pub fn new() -> Self {
        TypeVarGenerator { counter: 0 }
    }

    #[must_use]
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
    #[must_use]
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
    /// A closed anonymous record type with lexicographically sorted labels.
    Record(Vec<(String, InferType)>),
    /// A homogeneous array type.
    Array(Box<InferType>),
    /// A fixed-size array type `[T; N]`.
    SizedArray(Box<InferType>, u64),
    /// A shared pointer type.
    Reference(Box<InferType>),
    /// A mutable pointer type.
    MutReference(Box<InferType>),
    /// A named type (struct, enum) with type arguments.
    Named(String, Vec<InferType>),
}

impl InferType {
    #[must_use]
    pub fn int() -> Self {
        InferType::Concrete(Type::I64)
    }
    #[must_use]
    pub fn float() -> Self {
        InferType::Concrete(Type::F64)
    }
    #[must_use]
    pub fn bool() -> Self {
        InferType::Concrete(Type::Boolean)
    }
    #[must_use]
    pub fn str() -> Self {
        InferType::Concrete(Type::Str)
    }
    #[must_use]
    pub fn unit() -> Self {
        InferType::Concrete(Type::Unit)
    }
    #[must_use]
    pub fn never() -> Self {
        InferType::Never
    }
    #[allow(dead_code)] // public API used by typeinference test suite
    #[must_use]
    pub fn var(v: TypeVar) -> Self {
        InferType::Var(v)
    }
}

impl std::fmt::Display for InferType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InferType::Concrete(t) => write!(f, "{t}"),
            InferType::Var(v) => write!(f, "{v}"),
            InferType::Never => write!(f, "!"),
            InferType::Fun(params, ret) => {
                write!(f, "(")?;
                for (i, p) in params.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{p}")?;
                }
                write!(f, ") -> {ret}")
            }
            InferType::Tuple(ts) => {
                write!(f, "(")?;
                for (i, t) in ts.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{t}")?;
                }
                write!(f, ")")
            }
            InferType::Record(fields) => {
                write!(f, "{{ ")?;
                for (i, (name, ty)) in fields.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{name}: {ty}")?;
                }
                write!(f, " }}")
            }
            InferType::Array(t) => write!(f, "{t}[]"),
            InferType::SizedArray(t, n) => write!(f, "[{t}; {n}]"),
            InferType::Reference(t) => write!(f, "&{t}"),
            InferType::MutReference(t) => write!(f, "&var {t}"),
            InferType::Named(name, args) => {
                write!(f, "{name}")?;
                if !args.is_empty() {
                    write!(f, "<")?;
                    for (i, a) in args.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{a}")?;
                    }
                    write!(f, ">")?;
                }
                Ok(())
            }
        }
    }
}

/// Render one or more `InferType`s together for a single diagnostic message,
/// giving every distinct free `TypeVar` among them a stable, message-local
/// name (`?1`, `?2`, ...) instead of leaking `TypeVar`'s raw, globally
/// incrementing id (#266).
///
/// The label depends only on the *structure* of the types passed to one call,
/// in left-to-right depth-first encounter order — never on how much unrelated
/// inference ran earlier in the file, which is what made the raw id unstable
/// under an edit nowhere near the error site (`?t18` becoming `?t19` because
/// an unrelated struct was declared above it). The same `TypeVar` occurring in
/// more than one of `tys` gets the same local label in both, so `cannot unify
/// ?1 with ?1` (an occurs-check-shaped message) still reads as "these two are
/// the same still-unknown type," not two unrelated ones.
///
/// The other half of #266 — rendering a var under a *declared* generic
/// parameter's name, where one is known — is covered too, via `known_names`
/// (`TypeVar` → declared name, e.g. `InferContext::declared_var_names`).
/// Consulted before the local placeholder, so a struct/enum literal's own
/// type parameter shows as `T`, not `?1`. Only some instantiation sites tag
/// `known_names` today (struct/enum literal construction; see
/// `InferContext::tag_declared_var_name`'s callers) — a var with no tag
/// falls back to the placeholder exactly as before, so passing an empty map
/// reproduces the placeholder-only behavior unchanged.
#[must_use]
pub(crate) fn render_types(
    tys: &[&InferType],
    known_names: &HashMap<TypeVar, String>,
) -> Vec<String> {
    let mut local_names: HashMap<TypeVar, String> = HashMap::new();
    let mut next = 1usize;
    for ty in tys {
        collect_free_vars_in_order(ty, known_names, &mut local_names, &mut next);
    }
    tys.iter()
        .map(|ty| render_with_names(ty, known_names, &local_names))
        .collect()
}

fn collect_free_vars_in_order(
    ty: &InferType,
    known: &HashMap<TypeVar, String>,
    local: &mut HashMap<TypeVar, String>,
    next: &mut usize,
) {
    match ty {
        InferType::Var(v) => {
            if known.contains_key(v) {
                return;
            }
            local.entry(*v).or_insert_with(|| {
                let label = format!("?{next}");
                *next += 1;
                label
            });
        }
        InferType::Fun(params, ret) => {
            for p in params {
                collect_free_vars_in_order(p, known, local, next);
            }
            collect_free_vars_in_order(ret, known, local, next);
        }
        InferType::Tuple(ts) => {
            for t in ts {
                collect_free_vars_in_order(t, known, local, next);
            }
        }
        InferType::Record(fields) => {
            for (_, t) in fields {
                collect_free_vars_in_order(t, known, local, next);
            }
        }
        InferType::Array(t)
        | InferType::SizedArray(t, _)
        | InferType::Reference(t)
        | InferType::MutReference(t) => collect_free_vars_in_order(t, known, local, next),
        InferType::Named(_, args) => {
            for a in args {
                collect_free_vars_in_order(a, known, local, next);
            }
        }
        InferType::Concrete(_) | InferType::Never => {}
    }
}

fn render_with_names(
    ty: &InferType,
    known: &HashMap<TypeVar, String>,
    local: &HashMap<TypeVar, String>,
) -> String {
    match ty {
        InferType::Var(v) => known
            .get(v)
            .or_else(|| local.get(v))
            .cloned()
            .unwrap_or_else(|| ty.to_string()),
        InferType::Fun(params, ret) => format!(
            "({}) -> {}",
            params
                .iter()
                .map(|p| render_with_names(p, known, local))
                .collect::<Vec<_>>()
                .join(", "),
            render_with_names(ret, known, local)
        ),
        InferType::Tuple(ts) => format!(
            "({})",
            ts.iter()
                .map(|t| render_with_names(t, known, local))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        InferType::Record(fields) => format!(
            "{{ {} }}",
            fields
                .iter()
                .map(|(n, t)| format!("{n}: {}", render_with_names(t, known, local)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        InferType::Array(t) => format!("{}[]", render_with_names(t, known, local)),
        InferType::SizedArray(t, n) => format!("[{}; {n}]", render_with_names(t, known, local)),
        InferType::Reference(t) => format!("&{}", render_with_names(t, known, local)),
        InferType::MutReference(t) => format!("&var {}", render_with_names(t, known, local)),
        InferType::Named(name, args) if !args.is_empty() => format!(
            "{name}<{}>",
            args.iter()
                .map(|a| render_with_names(a, known, local))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        _ => ty.to_string(),
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
    #[must_use]
    pub fn new() -> Self {
        Substitution {
            bindings: HashMap::new(),
        }
    }

    /// Record that `var` maps to `ty`.
    ///
    /// An identity binding (`?v → ?v`) is a semantic no-op and is dropped: it can
    /// arise when `compose` resolves a chain back to its own key (e.g. composing
    /// `{a→b}` with `{b→a}`), and storing it would make `apply` recurse forever.
    pub fn bind(&mut self, var: TypeVar, ty: InferType) {
        if matches!(ty, InferType::Var(v) if v == var) {
            self.bindings.remove(&var);
            return;
        }
        self.bindings.insert(var, ty);
    }

    /// Look up the direct binding for `var`, if any.
    #[must_use]
    pub fn lookup(&self, var: TypeVar) -> Option<&InferType> {
        self.bindings.get(&var)
    }

    /// Recursively replace all type variables in `ty` using this substitution.
    #[must_use]
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
            InferType::Record(fields) => InferType::Record(
                fields
                    .iter()
                    .map(|(name, ty)| (name.clone(), self.apply(ty)))
                    .collect(),
            ),
            InferType::Array(t) => InferType::Array(Box::new(self.apply(t))),
            InferType::SizedArray(t, n) => InferType::SizedArray(Box::new(self.apply(t)), *n),
            InferType::Reference(t) => InferType::Reference(Box::new(self.apply(t))),
            InferType::MutReference(t) => InferType::MutReference(Box::new(self.apply(t))),
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
    #[must_use]
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

    /// Update this substitution in place so it becomes equivalent to
    /// `other ∘ self`, avoiding the temporary map allocation from `compose`.
    pub fn compose_in_place(&mut self, other: &Substitution) {
        for ty in self.bindings.values_mut() {
            *ty = other.apply(ty);
        }
        // Drop identity bindings (`?v → ?v`) that composition may have produced;
        // they are no-ops and would make `apply` recurse forever. See `bind`.
        self.bindings
            .retain(|var, ty| !matches!(ty, InferType::Var(v) if v == var));
        for (var, ty) in &other.bindings {
            self.bindings.entry(*var).or_insert_with(|| ty.clone());
        }
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
        InferType::Record(fields) => fields.iter().any(|(_, ty)| occurs_in(var, ty)),
        InferType::Array(t)
        | InferType::SizedArray(t, _)
        | InferType::Reference(t)
        | InferType::MutReference(t) => occurs_in(var, t),
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
            "occurs check failed: {var} occurs in {ty}"
        )));
    }
    let mut s = Substitution::new();
    s.bind(var, ty.clone());
    Ok(s)
}

/// Unify two inference types, returning a substitution that makes them equal.
///
/// # Errors
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
                Err(MetelError::internal(format!("cannot unify {a} with {b}")))
            }
        }
        (InferType::Var(v), _) => bind_var(*v, b),
        (_, InferType::Var(v)) => bind_var(*v, a),
        (InferType::Fun(params1, ret1), InferType::Fun(params2, ret2)) => {
            if params1.len() != params2.len() {
                return Err(MetelError::internal(format!("cannot unify {a} with {b}")));
            }
            let mut subst = Substitution::new();
            for (p1, p2) in params1.iter().zip(params2.iter()) {
                let s = unify(&subst.apply(p1), &subst.apply(p2))?;
                subst.compose_in_place(&s);
            }
            let s = unify(&subst.apply(ret1), &subst.apply(ret2))?;
            subst.compose_in_place(&s);
            Ok(subst)
        }
        (InferType::Tuple(ts1), InferType::Tuple(ts2)) => {
            if ts1.len() != ts2.len() {
                return Err(MetelError::internal(format!("cannot unify {a} with {b}")));
            }
            let mut subst = Substitution::new();
            for (t1, t2) in ts1.iter().zip(ts2.iter()) {
                let s = unify(&subst.apply(t1), &subst.apply(t2))?;
                subst.compose_in_place(&s);
            }
            Ok(subst)
        }
        (InferType::Record(fields1), InferType::Record(fields2)) => {
            if fields1.len() != fields2.len() {
                return Err(MetelError::internal(format!("cannot unify {a} with {b}")));
            }
            let mut subst = Substitution::new();
            for ((name1, ty1), (name2, ty2)) in fields1.iter().zip(fields2.iter()) {
                if name1 != name2 {
                    return Err(MetelError::internal(format!("cannot unify {a} with {b}")));
                }
                let s = unify(&subst.apply(ty1), &subst.apply(ty2))?;
                subst.compose_in_place(&s);
            }
            Ok(subst)
        }
        (InferType::SizedArray(t1, n1), InferType::SizedArray(t2, n2)) => {
            if n1 != n2 {
                return Err(MetelError::internal(format!("cannot unify {a} with {b}")));
            }
            unify(t1, t2)
        }
        // [T; N] coerces to T[] (one-directional)
        (InferType::Array(t1) | InferType::SizedArray(t1, _), InferType::Array(t2))
        | (InferType::Array(t1), InferType::SizedArray(t2, _))
        | (
            InferType::Reference(t1) | InferType::MutReference(t1),
            InferType::Reference(t2) | InferType::MutReference(t2),
        ) => unify(t1, t2),
        (InferType::Named(n1, args1), InferType::Named(n2, args2)) => {
            if n1 != n2 || args1.len() != args2.len() {
                return Err(MetelError::internal(format!("cannot unify {a} with {b}")));
            }
            let mut subst = Substitution::new();
            for (a1, a2) in args1.iter().zip(args2.iter()) {
                let s = unify(&subst.apply(a1), &subst.apply(a2))?;
                subst.compose_in_place(&s);
            }
            Ok(subst)
        }
        _ => Err(MetelError::internal(format!("cannot unify {a} with {b}"))),
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
    /// The binary operator this constraint came from, if any. Constraints are otherwise
    /// anonymous, so a failure could only ever report `cannot unify X with Y` — accurate
    /// but silent about *why* the two had to agree. Set for operand-agreement
    /// constraints so the failure can name the operator, which is the form
    /// `error-codes.md` documents for T0005.
    pub operator: Option<&'static str>,
}

impl Constraint {
    #[must_use]
    pub fn new(lhs: InferType, rhs: InferType, span: Span) -> Self {
        Self {
            lhs,
            rhs,
            span,
            operator: None,
        }
    }

    /// A constraint that exists because a binary operator requires its operands to agree.
    #[must_use]
    pub fn for_operator(lhs: InferType, rhs: InferType, span: Span, op: &'static str) -> Self {
        Self {
            lhs,
            rhs,
            span,
            operator: Some(op),
        }
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
/// Integer/float literal `TypeVars` are validated: if one resolves to a concrete
/// non-numeric type, T0001 is raised at the constraint site.
///
/// # Errors
/// Returns an error if any constraint fails to unify, or if an integer/float
/// literal type variable resolves to a non-numeric concrete type (T0001).
#[allow(dead_code)]
// kept as a standalone solver helper for tests and profiling comparisons
// Not generalized over `S: BuildHasher` -- this is single-binary interpreter
// code with one hasher throughout, never swapped; the generic bound would add
// noise with no real caller benefit.
#[allow(clippy::implicit_hasher)]
pub fn solve_constraints(
    constraints: Vec<Constraint>,
    integer_literal_vars: &HashSet<TypeVar>,
    float_literal_vars: &HashSet<TypeVar>,
) -> Result<Substitution, MetelError> {
    let mut subst = Substitution::new();
    let no_declared_names = HashMap::new();
    for c in constraints {
        apply_constraint(
            &mut subst,
            &c,
            integer_literal_vars,
            float_literal_vars,
            &no_declared_names,
        )?;
    }
    Ok(subst)
}

/// Format a failed operand-agreement constraint. When the constraint knows which operator
/// required the two sides to agree, report it the way `error-codes.md` documents for
/// T0005 — naming the operator — rather than the bare `cannot unify`, which says the two
/// types disagree but never says why they had to match in the first place.
fn operand_mismatch_error(
    constraint: &Constraint,
    lhs: &InferType,
    rhs: &InferType,
    known_names: &HashMap<TypeVar, String>,
) -> MetelError {
    let mut rendered = render_types(&[lhs, rhs], known_names);
    let rhs = rendered.pop().unwrap_or_else(|| rhs.to_string());
    let lhs = rendered.pop().unwrap_or_else(|| lhs.to_string());
    match constraint.operator {
        Some(op) => MetelError::type_error(
            crate::error::TypeErrorCode::T0005,
            format!("operator `{op}` cannot be applied to `{lhs}` and `{rhs}`"),
            &constraint.span,
        ),
        None => MetelError::type_error(
            crate::error::TypeErrorCode::T0001,
            format!("cannot unify {lhs} with {rhs}"),
            &constraint.span,
        ),
    }
}

/// As `operand_mismatch_error`, for a numeric literal bound to an incompatible type.
fn literal_mismatch_error(
    operator: Option<&'static str>,
    kind: &str,
    other: &InferType,
    span: &Span,
    known_names: &HashMap<TypeVar, String>,
) -> MetelError {
    let other = render_types(&[other], known_names)
        .pop()
        .unwrap_or_else(|| other.to_string());
    match operator {
        Some(op) => MetelError::type_error(
            crate::error::TypeErrorCode::T0005,
            format!("operator `{op}` cannot be applied to {kind} and `{other}`"),
            span,
        ),
        None => MetelError::type_error(
            crate::error::TypeErrorCode::T0001,
            format!("cannot unify {kind} with `{other}`"),
            span,
        ),
    }
}

fn apply_constraint(
    subst: &mut Substitution,
    constraint: &Constraint,
    integer_literal_vars: &HashSet<TypeVar>,
    float_literal_vars: &HashSet<TypeVar>,
    known_names: &HashMap<TypeVar, String>,
) -> Result<(), MetelError> {
    let lhs = subst.apply(&constraint.lhs);
    let rhs = subst.apply(&constraint.rhs);
    let solved = unify(&lhs, &rhs)
        .map_err(|_| operand_mismatch_error(constraint, &lhs, &rhs, known_names))?;
    subst.compose_in_place(&solved);
    validate_literal_bindings(
        constraint.operator,
        subst,
        integer_literal_vars,
        float_literal_vars,
        &constraint.span,
        known_names,
    )
}

/// Registry-aware counterpart of `apply_constraint`, used by `InferContext::solve`.
/// RFC-0078 §3.3: when a constraint's two sides are substituted to (by now, usually
/// fully concrete) types that don't unify directly, retry via inhabited-singleton
/// coercion before giving up — either side might name a singleton-coercible enum
/// reducible to the other side's type. This mirrors `construction.rs`'s
/// `maybe_singleton_coerce`, just at the constraint-solving level rather than at a
/// specific AST construction site, since a call's return type is often still an
/// unresolved type variable at the point its enclosing `let`/`return`/etc. records
/// its own constraint — only by the time `solve` substitutes and unifies is it
/// actually known to be a concrete, possibly singleton-coercible, enum type.
fn apply_constraint_with_coercion(
    subst: &mut Substitution,
    constraint: &Constraint,
    integer_literal_vars: &HashSet<TypeVar>,
    float_literal_vars: &HashSet<TypeVar>,
    opaque_return_vars: &HashSet<TypeVar>,
    registry: &TypeDefinitionRegistry,
    known_names: &HashMap<TypeVar, String>,
) -> Result<(), MetelError> {
    let lhs = subst.apply(&constraint.lhs);
    let rhs = subst.apply(&constraint.rhs);
    let solved = if let Ok(s) = unify(&lhs, &rhs) {
        s
    } else {
        let lhs_field = singleton_coerce_field_ty(registry, &lhs);
        let rhs_field = singleton_coerce_field_ty(registry, &rhs);
        let mk_err = || {
            let mut rendered = render_types(&[&lhs, &rhs], known_names);
            let rhs = rendered.pop().unwrap_or_else(|| rhs.to_string());
            let lhs = rendered.pop().unwrap_or_else(|| lhs.to_string());
            MetelError::type_error(
                crate::error::TypeErrorCode::T0001,
                format!("cannot unify {lhs} with {rhs}"),
                &constraint.span,
            )
        };
        if let (Some(lf), Some(rf)) = (&lhs_field, &rhs_field) {
            let s = unify(lf, rf).map_err(|_| mk_err())?;
            // `compose_in_place`'s merge keeps the *first* binding for any var
            // (first-write-wins), so a var already bound earlier to the raw
            // enum type (e.g. from the call's own return-type instantiation)
            // would never observe this coercion through composition alone.
            // Rebind directly: every later use of that var (including through
            // an environment binding that's itself just this var, unresolved
            // until use) then sees the coerced type instead of the raw enum.
            if let InferType::Var(v) = &constraint.lhs {
                subst.bind(*v, lf.clone());
            }
            if let InferType::Var(v) = &constraint.rhs {
                subst.bind(*v, rf.clone());
            }
            s
        } else if let Some(field_ty) = &lhs_field {
            let s = unify(field_ty, &rhs).map_err(|_| mk_err())?;
            if let InferType::Var(v) = &constraint.lhs {
                subst.bind(*v, field_ty.clone());
            }
            s
        } else if let Some(field_ty) = &rhs_field {
            let s = unify(&lhs, field_ty).map_err(|_| mk_err())?;
            if let InferType::Var(v) = &constraint.rhs {
                subst.bind(*v, field_ty.clone());
            }
            s
        } else {
            return Err(mk_err());
        }
    };
    subst.compose_in_place(&solved);
    validate_literal_bindings(
        constraint.operator,
        subst,
        integer_literal_vars,
        float_literal_vars,
        &constraint.span,
        known_names,
    )?;
    // RFC-0037: an opaque-return marker var may unify with another type
    // variable (the ordinary case for threading it through further generic
    // bounds, e.g. passing it to a function with its own `impl Aspect`
    // parameter) but never resolve to a genuinely concrete type — that would
    // let the caller "name" the concrete type the return value erases.
    // Checked right after THIS constraint's own composition, not once at the
    // very end of a whole solve(): by the time solving finishes, a legitimate
    // var-to-var chain (opaque marker -> some other function's own generic
    // parameter) is typically still just a `Var` at this point (that other
    // function's own body is solved separately, in its own `solve()` call),
    // so checking here can't confuse "resolved via legitimate indirection"
    // with "the concrete type was actually named" the way a single check
    // after full program-wide solving would.
    for &var in opaque_return_vars {
        let resolved = subst.apply(&InferType::Var(var));
        if !matches!(resolved, InferType::Var(_) | InferType::Never) {
            let resolved = render_types(&[&resolved], known_names)
                .pop()
                .unwrap_or_else(|| resolved.to_string());
            return Err(MetelError::type_error(
                crate::error::TypeErrorCode::T0018,
                format!(
                    "cannot name the concrete type of an opaque `impl Aspect` return value; use `impl Aspect` or a generic bound instead (resolved to `{resolved}`)"
                ),
                &constraint.span,
            ));
        }
    }
    Ok(())
}

/// RFC-0078 §3.2-§3.3: if `actual` names an enum with more than one variant,
/// exactly one of which is inhabited (all others have some field substituted to
/// `!`) with exactly one field, return that field's (substituted) type.
pub(crate) fn singleton_coerce_field_ty(
    registry: &TypeDefinitionRegistry,
    actual: &InferType,
) -> Option<InferType> {
    let (name, args): (&str, Vec<InferType>) = match actual {
        InferType::Concrete(Type::Named(n, targs)) => (
            n.as_str(),
            targs.iter().cloned().map(InferType::Concrete).collect(),
        ),
        InferType::Named(n, targs) => (n.as_str(), targs.clone()),
        _ => return None,
    };
    let enum_info = registry.enum_info(name)?;
    if enum_info.variants.len() <= 1 {
        return None;
    }
    let mut remap = Substitution::new();
    for (&tp, arg_ty) in enum_info.type_params.iter().zip(args.iter()) {
        remap.bind(tp, arg_ty.clone());
    }
    let mut inhabited: Option<InferType> = None;
    for v in &enum_info.variants {
        let uninhabited = v
            .fields
            .iter()
            .any(|f| matches!(remap.apply(&f.ty), InferType::Never));
        if uninhabited {
            continue;
        }
        if v.fields.len() != 1 || inhabited.is_some() {
            return None;
        }
        inhabited = Some(remap.apply(&v.fields[0].ty));
    }
    inhabited
}

fn validate_literal_bindings(
    operator: Option<&'static str>,
    subst: &Substitution,
    integer_literal_vars: &HashSet<TypeVar>,
    float_literal_vars: &HashSet<TypeVar>,
    span: &Span,
    known_names: &HashMap<TypeVar, String>,
) -> Result<(), MetelError> {
    for &var in integer_literal_vars {
        match subst.apply(&InferType::Var(var)) {
            InferType::Var(_) | InferType::Never => {}
            InferType::Concrete(t) if is_integer_type(&t) => {}
            other => {
                return Err(literal_mismatch_error(
                    operator,
                    "an integer literal",
                    &other,
                    span,
                    known_names,
                ))
            }
        }
    }
    for &var in float_literal_vars {
        match subst.apply(&InferType::Var(var)) {
            InferType::Var(_) | InferType::Never => {}
            InferType::Concrete(t) if is_float_type(&t) => {}
            other => {
                return Err(literal_mismatch_error(
                    operator,
                    "a float literal",
                    &other,
                    span,
                    known_names,
                ))
            }
        }
    }
    Ok(())
}

// ── Phase 6: Type Schemes ─────────────────────────────────────────────────────

/// Collect all type variables that appear free in `ty`.
fn collect_free_vars(ty: &InferType, vars: &mut HashSet<TypeVar>) {
    match ty {
        InferType::Concrete(_) | InferType::Never => {}
        InferType::Var(v) => {
            vars.insert(*v);
        }
        InferType::Fun(params, ret) => {
            for p in params {
                collect_free_vars(p, vars);
            }
            collect_free_vars(ret, vars);
        }
        InferType::Tuple(ts) | InferType::Named(_, ts) => {
            for t in ts {
                collect_free_vars(t, vars);
            }
        }
        InferType::Record(fields) => {
            for (_, ty) in fields {
                collect_free_vars(ty, vars);
            }
        }
        InferType::Array(t)
        | InferType::SizedArray(t, _)
        | InferType::Reference(t)
        | InferType::MutReference(t) => collect_free_vars(t, vars),
    }
}

#[must_use]
pub fn free_vars(ty: &InferType) -> HashSet<TypeVar> {
    let mut vars = HashSet::new();
    collect_free_vars(ty, &mut vars);
    vars
}

/// A universally quantified type: `∀ quantified_vars. ty`.
///
/// Variables in `quantified_vars` are locally owned — each use site gets
/// fresh copies via `instantiate`, enabling let-polymorphism.
///
/// `param_names` optionally holds the source-level names of each quantified variable,
/// in the same sorted order as `quantified_vars`. Used during construction-at-call-time
/// to resolve type annotations like `T[]` inside generic function bodies.
#[derive(Debug, Clone)]
pub struct TypeScheme {
    pub quantified_vars: Vec<TypeVar>,
    /// Source-level names for quantified vars (same order). Empty for builtins.
    pub param_names: Vec<String>,
    /// Positive bounds per quantified var (same order; empty Vec = unbounded).
    /// Bounds travel WITH the scheme so they survive prelude derivation and
    /// the export alpha-renaming, unlike the TypeVar-keyed `fun_bounds`
    /// registry (which only works within the defining module).
    pub bounds: Vec<Vec<GenericBound>>,
    /// Negative bounds per quantified var (same order as `quantified_vars`).
    pub neg_bounds: Vec<Vec<GenericBound>>,
    /// Record-kinded flags per quantified var (same order as `quantified_vars`).
    pub record_kinds: Vec<bool>,
    /// Per-quantified-var projection metadata (RFC-0082). Index-aligned with
    /// `quantified_vars`. `Some((position, aspect_name, assoc_name, placeholder_tv))` means the
    /// i-th quantified var has a projection `T::AssocName` through `aspect_name`.
    /// `placeholder_tv` is the original `TypeVar` of the projection placeholder (before renaming),
    /// used at instantiation time to find the fresh copy and bind it.
    /// `None` means no projection declared for this position. The `position` is
    /// the 0-based index into `quantified_vars` (redundant but explicit).
    pub assoc_projections: Vec<Option<(usize, String, String, TypeVar)>>,
    /// Per-quantified-var equality constraints (RFC-0082 §4).
    /// `assoc_eq_constraints[i]` lists `(left_proj, right_proj, type)` constraints
    /// where both sides resolve to the i-th quantified var's projection.
    pub assoc_eq_constraints: Vec<Vec<(String, String, InferType)>>,
    /// Per-quantified-var opaque-return metadata (RFC-0037). Index-aligned with
    /// `quantified_vars`. `Some((aspect_name, concrete_ty))` means the i-th
    /// quantified var is a return-position `impl Aspect` occurrence whose concrete
    /// type is fixed by the function's own body (not chosen per call, unlike an
    /// ordinary generic). The caller never sees `concrete_ty` directly — used only
    /// to (a) verify the aspect bound once at definition time, (b) let construction
    /// build a concrete `Type` for the call expression and the function's own
    /// eagerly-built body. `None` means no opaque return at this position.
    pub opaque_returns: Vec<Option<(String, Type)>>,
    pub ty: InferType,
}

impl TypeScheme {
    /// A monomorphic scheme — no quantified variables.
    #[must_use]
    pub fn mono(ty: InferType) -> Self {
        Self {
            quantified_vars: vec![],
            param_names: vec![],
            bounds: vec![],
            neg_bounds: vec![],
            record_kinds: vec![],
            assoc_projections: vec![],
            assoc_eq_constraints: vec![],
            opaque_returns: vec![],
            ty,
        }
    }

    /// Attach per-var aspect bounds, given a `TypeVar` → bounds map. Robust to
    /// quantifier ordering: each quantified var looks up its own entry.
    #[must_use]
    pub fn with_bounds(
        mut self,
        by_var: &std::collections::HashMap<TypeVar, Vec<GenericBound>>,
    ) -> Self {
        if by_var.values().all(std::vec::Vec::is_empty) {
            return self;
        }
        self.bounds = self
            .quantified_vars
            .iter()
            .map(|v| by_var.get(v).cloned().unwrap_or_default())
            .collect();
        self
    }

    /// Attach per-var negative aspect bounds, mirroring `with_bounds`.
    #[must_use]
    pub fn with_neg_bounds(
        mut self,
        by_var: &std::collections::HashMap<TypeVar, Vec<GenericBound>>,
    ) -> Self {
        if by_var.values().all(std::vec::Vec::is_empty) {
            return self;
        }
        self.neg_bounds = self
            .quantified_vars
            .iter()
            .map(|v| by_var.get(v).cloned().unwrap_or_default())
            .collect();
        self
    }

    #[must_use]
    pub fn with_record_kinds(mut self, by_var: &std::collections::HashMap<TypeVar, bool>) -> Self {
        if by_var.values().all(|flag| !*flag) {
            return self;
        }
        self.record_kinds = self
            .quantified_vars
            .iter()
            .map(|v| by_var.get(v).copied().unwrap_or(false))
            .collect();
        self
    }

    /// Attach per-quantified-var associated-type projection metadata (RFC-0082).
    /// Each entry in `proj_map` maps a quantified `TypeVar` to its projection info
    /// including the placeholder `TypeVar` for the projection.
    #[must_use]
    pub fn with_assoc_projections(
        mut self,
        proj_map: &std::collections::HashMap<TypeVar, (usize, String, String, TypeVar)>,
    ) -> Self {
        if proj_map.is_empty() {
            return self;
        }
        self.assoc_projections = self
            .quantified_vars
            .iter()
            .enumerate()
            .map(|(i, v)| {
                proj_map
                    .get(v)
                    .cloned()
                    .or_else(|| proj_map.values().find(|(pos, _, _, _)| *pos == i).cloned())
            })
            .collect();
        self
    }

    /// Attach per-var equality constraints (RFC-0082 §4), given a `TypeVar` →
    /// constraints map. Mirrors `with_bounds`/`with_neg_bounds`: robust to
    /// quantifier ordering, each quantified var looks up its own entry.
    #[must_use]
    pub fn with_assoc_eq_constraints(mut self, by_var: &AssocEqConstraints) -> Self {
        if by_var.values().all(std::vec::Vec::is_empty) {
            return self;
        }
        self.assoc_eq_constraints = self
            .quantified_vars
            .iter()
            .map(|v| by_var.get(v).cloned().unwrap_or_default())
            .collect();
        self
    }

    /// Attach per-var opaque-return metadata (RFC-0037), given a `TypeVar` →
    /// `(aspect_name, concrete_type)` map. Mirrors `with_bounds`/`with_neg_bounds`:
    /// robust to quantifier ordering, each quantified var looks up its own entry.
    #[must_use]
    pub fn with_opaque_returns(
        mut self,
        by_var: &std::collections::HashMap<TypeVar, (String, Type)>,
    ) -> Self {
        if by_var.is_empty() {
            return self;
        }
        self.opaque_returns = self
            .quantified_vars
            .iter()
            .map(|v| by_var.get(v).cloned())
            .collect();
        self
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
                write!(f, "{v}")?;
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
#[must_use]
// See `solve_constraints` above for why hasher-generalization isn't worthwhile here.
#[allow(clippy::implicit_hasher)]
pub fn generalize(ty: InferType, env_free_vars: &HashSet<TypeVar>) -> TypeScheme {
    let mut quantified: Vec<TypeVar> = free_vars(&ty).difference(env_free_vars).copied().collect();
    quantified.sort();
    TypeScheme {
        quantified_vars: quantified,
        param_names: vec![],
        bounds: vec![],
        neg_bounds: vec![],
        record_kinds: vec![],
        assoc_projections: vec![],
        assoc_eq_constraints: vec![],
        opaque_returns: vec![],
        ty,
    }
}

/// Like `generalize` but also records the source-level name for each quantified variable.
/// `name_map` maps `TypeVar` ID → param name (e.g. `{5 → "T"}`).
#[must_use]
#[allow(clippy::implicit_hasher)]
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
/// `TypeVar` to the fresh `TypeVar` it was replaced with.
pub fn instantiate_with_renaming(
    scheme: &TypeScheme,
    gen: &mut TypeVarGenerator,
) -> (InferType, HashMap<TypeVar, TypeVar>) {
    let mut renaming = HashMap::with_capacity(scheme.quantified_vars.len());
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
/// That `SymbolId` is the key stored in `TypedImplBlock::aspect_id` and in
/// `RuntimeAspectImpl::aspect_id`.  The elaboration pass has no other dependency on this
/// registry; it does not read or write inference-phase state.
/// RFC-0082 §4 equality constraints for one generic function/type var: a list
/// of `(aspect, assoc_name, expected_type)` triples.
pub type AssocEqConstraints = HashMap<TypeVar, Vec<(String, String, InferType)>>;
#[derive(Debug, Clone)]
pub enum GenericBound {
    Aspect(String),
    Row(RowConstraint),
}

#[derive(Debug, Clone)]
pub struct RowConstraint {
    pub fields: Vec<RowConstraintField>,
    pub open: bool,
}

#[derive(Debug, Clone)]
pub struct RowConstraintField {
    pub label: String,
    pub ty: Option<TypeExpr>,
}

impl From<&RowBound> for RowConstraint {
    fn from(value: &RowBound) -> Self {
        Self {
            fields: value
                .fields
                .iter()
                .map(|field| RowConstraintField {
                    label: field.label.clone(),
                    ty: field.ty.clone(),
                })
                .collect(),
            open: value.open,
        }
    }
}

impl GenericBound {
    #[must_use]
    pub fn from_ast(bound: &crate::ast::Bound) -> Option<Self> {
        if let Some(row) = bound.row_bound() {
            return Some(Self::Row(RowConstraint::from(row)));
        }
        match &bound.head {
            crate::ast::BoundHead::Aspect(TypeExpr::Named(name, _)) => {
                Some(Self::Aspect(name.clone()))
            }
            _ => None,
        }
    }

    #[must_use]
    pub fn aspect_name(&self) -> Option<&str> {
        match self {
            Self::Aspect(name) => Some(name.as_str()),
            Self::Row(_) => None,
        }
    }
}

impl fmt::Display for GenericBound {
    // One dispatch table over TypeExpr so a bound prints exactly as written.
    #[allow(clippy::too_many_lines)]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fn write_type_expr(f: &mut fmt::Formatter<'_>, ty: &TypeExpr) -> fmt::Result {
            match ty {
                TypeExpr::Named(name, args) => {
                    f.write_str(name)?;
                    if !args.is_empty() {
                        f.write_str("<")?;
                        for (index, arg) in args.iter().enumerate() {
                            if index > 0 {
                                f.write_str(", ")?;
                            }
                            write_type_expr(f, arg)?;
                        }
                        f.write_str(">")?;
                    }
                    Ok(())
                }
                TypeExpr::Unit => f.write_str("unit"),
                TypeExpr::Tuple(items) => {
                    f.write_str("(")?;
                    for (index, item) in items.iter().enumerate() {
                        if index > 0 {
                            f.write_str(", ")?;
                        }
                        write_type_expr(f, item)?;
                    }
                    f.write_str(")")
                }
                TypeExpr::Record(fields) => {
                    f.write_str("{ ")?;
                    for (index, (label, item_ty)) in fields.iter().enumerate() {
                        if index > 0 {
                            f.write_str(", ")?;
                        }
                        f.write_str(label)?;
                        f.write_str(": ")?;
                        write_type_expr(f, item_ty)?;
                    }
                    f.write_str(" }")
                }
                TypeExpr::Array(inner) => {
                    write_type_expr(f, inner)?;
                    f.write_str("[]")
                }
                TypeExpr::SizedArray(inner, size) => {
                    write_type_expr(f, inner)?;
                    write!(f, "[{size}]")
                }
                TypeExpr::Reference(inner) => {
                    f.write_str("&")?;
                    write_type_expr(f, inner)
                }
                TypeExpr::MutReference(inner) => {
                    f.write_str("&var ")?;
                    write_type_expr(f, inner)
                }
                TypeExpr::Fun(params, ret) => {
                    f.write_str("fun(")?;
                    for (index, param) in params.iter().enumerate() {
                        if index > 0 {
                            f.write_str(", ")?;
                        }
                        write_type_expr(f, param)?;
                    }
                    f.write_str(")")?;
                    if let Some(ret) = ret {
                        f.write_str(" -> ")?;
                        write_type_expr(f, ret)?;
                    }
                    Ok(())
                }
                TypeExpr::ImplAspect { bound, .. } => {
                    f.write_str("impl ")?;
                    write_type_expr(f, bound)
                }
                TypeExpr::Projection {
                    base, assoc_name, ..
                } => {
                    write_type_expr(f, base)?;
                    write!(f, "::{assoc_name}")
                }
                TypeExpr::RecordProjection { path, fields, .. } => {
                    write!(f, "{}.{{ {} }}", path.join("::"), fields.join(", "))
                }
            }
        }

        match self {
            Self::Aspect(name) => f.write_str(name),
            Self::Row(row) => {
                f.write_str("{ ")?;
                for (index, field) in row.fields.iter().enumerate() {
                    if index > 0 {
                        f.write_str(", ")?;
                    }
                    f.write_str(&field.label)?;
                    if let Some(ty) = &field.ty {
                        f.write_str(": ")?;
                        write_type_expr(f, ty)?;
                    }
                }
                if row.open {
                    if !row.fields.is_empty() {
                        f.write_str(", ")?;
                    }
                    f.write_str("..")?;
                }
                f.write_str(" }")
            }
        }
    }
}
/// Memo table for `InferContext::fresh_assoc_projection_var`: `(base_tv, aspect,
/// assoc_name)` -> the placeholder `TypeVar` already minted for that projection.
pub type AssocProjectionMemo = HashMap<(TypeVar, String, String), TypeVar>;
/// Insertion-order log of every projection placeholder minted during one
/// function/method body: `(base_tv, aspect, assoc_name, placeholder_tv)`.
pub type AssocProjectionLog = Vec<(TypeVar, String, String, TypeVar)>;
/// One conditional impl's per-position bound requirements: `(pos_bounds, neg_bounds)`,
/// see `TypeDefinitionRegistry::conditional_impl_bounds`.
pub type ConditionalImplBoundEntry = (Vec<Vec<GenericBound>>, Vec<Vec<GenericBound>>);
/// One registered method scheme variant: `(scheme, struct_tvars, aspect_name)`.
/// `aspect_name` is `None` for an inherent (non-aspect) method -- see
/// `TypeDefinitionRegistry::method_scheme_variants`. Carrying the aspect name
/// per variant (issue #272) lets a caller that picks a candidate by bound
/// satisfaction also stamp the winning aspect onto the call site's dispatch
/// mode, instead of leaving it `Dynamic` for a later, bound-unaware pass to
/// mis-resolve.
pub type MethodSchemeVariant = (TypeScheme, Vec<TypeVar>, Option<String>);
/// One registered array method scheme variant: `(scheme, element_tvars, aspect_name)`.
/// See `MethodSchemeVariant`'s doc for why `aspect_name` is carried here too.
pub type ArrayMethodSchemeVariant = (TypeScheme, Vec<TypeVar>, Option<String>);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VisibleTypeKind {
    Struct,
    Enum,
}

/// Embed a concrete `Type` into inference space.
///
/// Structural rather than a blanket `InferType::Concrete` wrap: a named type
/// becomes `InferType::Named` with embedded arguments, so that one canonical
/// shape exists for each type regardless of which side it came from. The
/// aspect-satisfaction query below relies on that — it matches on
/// `InferType::Named`, and would miss a `Concrete(Type::Named(..))`.
#[must_use]
pub fn type_to_infer(ty: &Type) -> InferType {
    match ty {
        Type::Never => InferType::Never,
        Type::Array(t) => InferType::Array(Box::new(type_to_infer(t))),
        Type::SizedArray(t, n) => InferType::SizedArray(Box::new(type_to_infer(t)), *n),
        Type::Tuple(ts) => InferType::Tuple(ts.iter().map(type_to_infer).collect()),
        Type::Record(fields) => InferType::Record(
            fields
                .iter()
                .map(|(name, ty)| (name.clone(), type_to_infer(ty)))
                .collect(),
        ),
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

/// Aspects assumed to hold for abstract type parameters, keyed by the type
/// variable standing for the parameter.
///
/// Keyed by `TypeVar`, not by name. A type parameter is not a named type that
/// happens to be spelled `T` — it is structurally a different thing, and
/// `InferType::Var` is where that distinction already lives. Keying on the
/// name conflated the two: a real `struct T` in scope would match an entry
/// meant for a parameter called `T` and inherit its assumed aspects.
pub type AspectAssumptions = HashMap<TypeVar, std::collections::HashSet<String>>;

#[derive(Debug, Clone)]
pub struct TypeDefinitionRegistry {
    /// struct name → fields with declaration spans.
    struct_env: HashMap<String, Vec<FieldEntry>>,
    /// struct name → declaring module path.
    struct_decl_modules: HashMap<String, Vec<String>>,
    /// Ordered type-parameter `TypeVars` per generic struct (absent for non-generic structs).
    struct_type_params: HashMap<String, Vec<TypeVar>>,
    /// Ordered type-parameter names per generic struct/enum. Parallel to `struct_type_params`.
    /// Used when setting up impl method scopes so param names resolve to `TypeVars`.
    struct_generic_names: HashMap<String, Vec<String>>,
    /// Polymorphic method schemes for methods on generic structs that reference the struct's
    /// type params. Key: (`type_name`, `method_name`) → (scheme, `struct_tvars_ordered`).
    /// `struct_tvars_ordered`[i] corresponds to the i-th type arg of the receiver at the call site.
    method_scheme_env: HashMap<String, HashMap<String, (TypeScheme, Vec<TypeVar>)>>,
    /// RFC-0036 §3.1: multiple conditional impls of the same aspect for the same struct
    /// providing the same method name. Key: (`type_name`, `method_name`) → Vec of
    /// (scheme, `struct_tvars`). `register_method_scheme_variant` pushes; `method_scheme_for`
    /// (singular) keeps returning the last-registered entry for backward compatibility.
    /// NOTE: nothing currently reads this list back to disambiguate between variants —
    /// see the open question flagged in commit e20718e / issue #264.
    method_scheme_variants: HashMap<String, HashMap<String, Vec<MethodSchemeVariant>>>,
    /// Method schemes for structural array targets (`impl<T> Aspect for T[]`). The
    /// pinned vars correspond to the receiver array's element type positions.
    array_method_scheme_env: HashMap<String, (TypeScheme, Vec<TypeVar>)>,
    /// Variant list mirroring `method_scheme_variants` for array-target impls.
    array_method_scheme_variants: HashMap<String, Vec<ArrayMethodSchemeVariant>>,
    /// Exact generic method scheme keyed by the method declaration's source span.
    /// Unlike the name-keyed environments above, this remains unambiguous when
    /// several conditional impls provide the same method name.
    generic_method_schemes_by_span: HashMap<Span, TypeScheme>,
    /// Per-type-param aspect bounds for generic structs and enums.
    /// Key: type name. Value: one Vec<String> per type param (same order as `struct_type_params`),
    /// each containing the aspect names that param must satisfy.
    type_param_bounds: HashMap<String, Vec<Vec<GenericBound>>>,
    /// Negative per-type-param aspect bounds (`T: !Aspect`) for generic structs and enums.
    /// Key: type name. Value: one Vec<String> per type param, each containing the
    /// aspect names that param must NOT satisfy (RFC-0072, issue #243).
    neg_type_param_bounds: HashMap<String, Vec<Vec<GenericBound>>>,
    /// Record-kinded flags for generic struct/enum params, keyed by type name and ordered
    /// to match `struct_type_params`.
    type_param_record_kinds: HashMap<String, Vec<bool>>,
    /// Aspect bounds per generic function. Key: function name.
    /// Value: map from each quantified `TypeVar` to the list of required aspect names.
    fun_bounds: HashMap<String, HashMap<TypeVar, Vec<GenericBound>>>,
    /// Negative aspect bounds per generic function (`T: !Aspect`). Key: function name.
    /// Value: map from each quantified `TypeVar` to the list of negated aspect names
    /// (RFC-0072, issue #243).
    neg_fun_bounds: HashMap<String, HashMap<TypeVar, Vec<GenericBound>>>,
    /// Record-kinded flags per generic function/type var.
    fun_record_kinds: HashMap<String, HashMap<TypeVar, bool>>,
    /// RFC-0082 §4: associated-type equality constraints per generic function.
    /// Key: function name. Value: map from each quantified `TypeVar` to the list of
    /// `(aspect, assoc_name, expected_type)` equality constraints.
    fun_assoc_eq_constraints: HashMap<String, AssocEqConstraints>,
    /// Tracks which struct names were registered in each lexical scope so they
    /// can be removed on scope exit. Empty when outside any scoped block.
    struct_scope_stack: Vec<Vec<String>>,
    method_env: HashMap<String, HashMap<String, InferType>>,
    method_receiver_env: HashMap<String, HashMap<String, ReceiverKind>>,
    array_method_env: HashMap<String, InferType>,
    array_method_receiver_env: HashMap<String, ReceiverKind>,
    enum_env: HashMap<String, EnumInfo>,
    variant_declaring_enums: HashMap<String, Vec<String>>,
    /// enum name → declaring module path.
    enum_decl_modules: HashMap<String, Vec<String>>,
    /// aspect name → ordered list of method names the aspect declares.
    /// Used to verify impl blocks are complete.
    aspect_env: HashMap<String, Vec<String>>,
    /// aspect name → declaring module path.
    aspect_decl_modules: HashMap<String, Vec<String>>,
    /// aspect name → ordered generic parameter names declared by the aspect.
    aspect_generics: HashMap<String, Vec<String>>,
    /// aspect name → full declared methods, including default bodies.
    aspect_method_defs: HashMap<String, Vec<AspectMethod>>,
    /// Move-check reconstruction only: symbolic nominal placeholders and the
    /// aspect bounds already proved for their source generic parameters.
    symbolic_named_aspects: HashMap<String, HashSet<String>>,
    /// (`target_type_id`, `aspect_name`) → list of type-arg vectors, one per registered
    /// impl. E.g. (Int's id, "From") → [[`Type::F64`]] means `impl From<Float> for Int`.
    ///
    /// Target is keyed by `SymbolId`, not name (ADR-0042/issue #239): two modules each
    /// declaring a *type* with the same surface name must never conflate their impls,
    /// the same collision class ADR-0041 already fixed for runtime dispatch. Resolving
    /// a name to its id (`resolve_type_position_id`) needs to know which module the
    /// name is being read *from* — an impl's target type is very often imported, not
    /// locally declared — so this registry carries its own copies of the global symbol
    /// table and every module's import scope (both `Rc`-shared, set once when built) to
    /// do that resolution the same way `reference_resolver` does for expression
    /// `Ident`s.
    ///
    /// The **aspect stays name-keyed, deliberately** — unlike types, `From`/`Iterable`
    /// (and aspect names generally, for this bookkeeping) are treated as shared,
    /// program-wide protocol slots, not shadowable per-module declarations: a module
    /// declaring its own `aspect From<T>` for a domain conversion (e.g.
    /// `evaluator/types/60_from_cast.mtl`'s `Celsius`/`Fahrenheit`) still needs the
    /// *built-in* numeric `From` cross-product (`i64 as f64`) to resolve in the same
    /// file, registered from `std::core`'s own scope where "From" means the builtin.
    /// Resolving the aspect half through the same shadowing-aware lookup as the target
    /// would make a local `From`/`Iterable` declaration invisibly shadow the builtin
    /// one for this bookkeeping — a real regression caught by that exact test.
    impl_aspect_env: HashMap<(SymbolId, String), Vec<Vec<Type>>>,
    /// RFC-0036 §2.2/§3.1: per-conditional-impl bound metadata.
    /// Key: `(target_type_id, aspect_name)`. Value: Vec of `(pos_bounds, neg_bounds)`
    /// where `pos_bounds[i]` / `neg_bounds[i]` are the aspect names required / forbidden
    /// at the i-th type-argument position of the target type, for ONE conditional impl.
    /// Populated INSTEAD OF `impl_aspect_env` when `is_generic_target && (impl_bounds ||
    /// impl_neg_bounds non-empty)` — see `register_conditional_impl_bounds`.
    conditional_impl_bounds: HashMap<(SymbolId, String), Vec<ConditionalImplBoundEntry>>,
    /// Bare-parameter blanket impl metadata (`impl<T> Aspect for T`), keyed by
    /// aspect name alone because the target has no nominal head to resolve.
    bare_impl_bounds: HashMap<String, Vec<ConditionalImplBoundEntry>>,
    /// Conditional impl metadata for structural array targets (`impl<T: Bound> Aspect for T[]`).
    array_impl_bounds: HashMap<String, Vec<ConditionalImplBoundEntry>>,
    /// Generic negative impl metadata, keyed by `(target_type_id, aspect_name)`.
    /// Mirrors `conditional_impl_bounds`, but a matching entry means the aspect is
    /// explicitly absent for that instantiation (`impl<T> !Aspect for Foo<T> {}`),
    /// so `type_satisfies_aspect` must return false before consulting positive impls.
    neg_conditional_impl_bounds: HashMap<(SymbolId, String), Vec<ConditionalImplBoundEntry>>,
    /// Bare-parameter negative blanket impl metadata (`impl<T> !Aspect for T`).
    bare_neg_impl_bounds: HashMap<String, Vec<ConditionalImplBoundEntry>>,
    /// Negative conditional impl metadata for structural array targets.
    array_neg_impl_bounds: HashMap<String, Vec<ConditionalImplBoundEntry>>,
    /// RFC-0060 §5 / issue #244: concrete negative impls, keyed by
    /// `(target_type_id, aspect_name)`. Value: one `Vec<Type>` per registered
    /// negative impl, the target's own concrete type args (e.g. `[i64]` for
    /// `impl !Marker for Foo<i64> {}`) — consulted by `type_satisfies_aspect` to let
    /// an explicit negative impl override a blanket positive impl for this exact
    /// instantiation.
    neg_impl_env: HashMap<(SymbolId, String), Vec<Vec<Type>>>,
    /// Global `(module, name) -> SymbolId` table. See `impl_aspect_env`'s doc.
    symbols: Rc<HashMap<(Vec<String>, String), SymbolId>>,
    /// Every module's resolved import scope. See `impl_aspect_env`'s doc.
    scopes: Rc<HashMap<Vec<String>, ModuleScope>>,
    /// Aspect name → its declared associated-type members (name + optional bound), RFC-0082 §1.
    aspect_assoc_type_decls: HashMap<String, Vec<AssocTypeDecl>>,
    /// (`target_type_id`, `aspect_name`) → assoc-type-name → concrete Type, RFC-0082 §2.
    /// Populated only for concrete (non-generic) impls.
    impl_assoc_types: HashMap<(SymbolId, String), HashMap<String, Type>>,
}

impl TypeDefinitionRegistry {
    fn split_qualified_type_name(name: &str) -> Option<(Vec<String>, &str)> {
        let (module_path, short_name) = name.rsplit_once("::")?;
        Some((
            module_path
                .split("::")
                .map(std::string::ToString::to_string)
                .collect(),
            short_name,
        ))
    }

    fn resolve_struct_name_from_projection(
        &self,
        current_module: &[String],
        type_name: &str,
    ) -> Option<String> {
        self.visible_decl_name(current_module, type_name, &self.struct_env)
    }

    pub(crate) fn visible_type_kind(
        &self,
        current_module: &[String],
        type_name: &str,
    ) -> Option<VisibleTypeKind> {
        if self
            .resolve_struct_name_from_projection(current_module, type_name)
            .is_some()
        {
            return Some(VisibleTypeKind::Struct);
        }
        if self
            .visible_decl_name(current_module, type_name, &self.enum_env)
            .is_some()
        {
            return Some(VisibleTypeKind::Enum);
        }
        None
    }

    pub(crate) fn projection_struct_fields(
        &self,
        current_module: &[String],
        type_name: &str,
    ) -> Option<(&str, &Vec<FieldEntry>)> {
        let resolved_name = self.resolve_struct_name_from_projection(current_module, type_name)?;
        self.struct_env
            .get_key_value(&resolved_name)
            .map(|(name, fields)| (name.as_str(), fields))
    }

    #[must_use]
    pub fn new() -> Self {
        Self {
            struct_env: HashMap::new(),
            struct_decl_modules: HashMap::new(),
            struct_type_params: HashMap::new(),
            struct_generic_names: HashMap::new(),
            method_scheme_env: HashMap::new(),
            method_scheme_variants: HashMap::new(),
            array_method_scheme_env: HashMap::new(),
            array_method_scheme_variants: HashMap::new(),
            generic_method_schemes_by_span: HashMap::new(),
            type_param_bounds: HashMap::new(),
            neg_type_param_bounds: HashMap::new(),
            type_param_record_kinds: HashMap::new(),
            fun_bounds: HashMap::new(),
            neg_fun_bounds: HashMap::new(),
            fun_record_kinds: HashMap::new(),
            fun_assoc_eq_constraints: HashMap::new(),
            struct_scope_stack: Vec::new(),
            method_env: HashMap::new(),
            method_receiver_env: HashMap::new(),
            array_method_env: HashMap::new(),
            array_method_receiver_env: HashMap::new(),
            enum_env: HashMap::new(),
            variant_declaring_enums: HashMap::new(),
            enum_decl_modules: HashMap::new(),
            aspect_env: HashMap::new(),
            aspect_decl_modules: HashMap::new(),
            aspect_generics: HashMap::new(),
            aspect_method_defs: HashMap::new(),
            symbolic_named_aspects: HashMap::new(),
            impl_aspect_env: HashMap::new(),
            conditional_impl_bounds: HashMap::new(),
            bare_impl_bounds: HashMap::new(),
            array_impl_bounds: HashMap::new(),
            neg_conditional_impl_bounds: HashMap::new(),
            bare_neg_impl_bounds: HashMap::new(),
            array_neg_impl_bounds: HashMap::new(),
            neg_impl_env: HashMap::new(),
            symbols: Rc::new(HashMap::new()),
            scopes: Rc::new(HashMap::new()),
            aspect_assoc_type_decls: HashMap::new(),
            impl_assoc_types: HashMap::new(),
        }
    }

    /// Give this registry the global symbol table and import scopes it needs to
    /// resolve impl target/aspect names to ids (see `impl_aspect_env`'s doc). Set once,
    /// right after `build_registry` constructs a fresh registry for a module; cheap to
    /// call repeatedly (an `Rc` clone, not a deep copy of either map).
    pub fn set_symbol_resolution(
        &mut self,
        symbols: Rc<HashMap<(Vec<String>, String), SymbolId>>,
        scopes: Rc<HashMap<Vec<String>, ModuleScope>>,
    ) {
        self.symbols = symbols;
        self.scopes = scopes;
    }

    /// Resolve a type-position name (an impl's target type or aspect name) to its
    /// declaring `SymbolId`, from `current_module`'s point of view. Mirrors
    /// `reference_resolver::resolve_name`'s precedence for expression `Ident`s — local
    /// declaration, then explicit import, then glob imports (user tier before std) —
    /// applied to type positions, which have no resolver of their own otherwise.
    /// `None` if `name` isn't visible in `current_module` at all (or symbol resolution
    /// hasn't been wired up — the single-program/no-resolver path, if it's ever used
    /// with this registry, degrades to no impl-aspect tracking rather than panicking).
    fn resolve_type_position_id(&self, current_module: &[String], name: &str) -> Option<SymbolId> {
        if let Some(id) = self
            .symbols
            .get(&(current_module.to_vec(), name.to_string()))
        {
            return Some(*id);
        }
        let scope = self.scopes.get(current_module)?;
        if let Some(binding) = scope.explicit.get(name) {
            return Some(binding.symbol_id);
        }
        let mut std_hit = None;
        for (tier, glob_module) in &scope.globs {
            if let Some(id) = self.symbols.get(&(glob_module.clone(), name.to_string())) {
                match tier {
                    GlobTier::User => return Some(*id),
                    GlobTier::Std => std_hit = std_hit.or(Some(*id)),
                }
            }
        }
        if let Some(id) = std_hit {
            return Some(id);
        }

        // A qualified annotation can name an imported module handle. Resolve that
        // handle (including aliases), then consult either the module's declaration
        // table or its re-export surface. This mirrors ordinary module-path lookup:
        // `facade::Token` is valid when `facade` re-exports `Token` from another
        // module, even though no `(facade, Token)` declaration exists in `symbols`.
        let (prefix, short_name) = Self::split_qualified_type_name(name)?;
        let (first, rest) = prefix.split_first()?;
        let binding = scope.explicit.get(first)?;
        if !matches!(binding.kind, crate::name_resolver::BindingKind::Module) {
            return None;
        }
        let mut module_path = binding.source_module.clone();
        module_path.extend_from_slice(rest);
        self.symbols
            .get(&(module_path.clone(), short_name.to_string()))
            .copied()
            .or_else(|| {
                self.scopes
                    .get(&module_path)
                    .and_then(|module_scope| module_scope.re_exports.get(short_name))
                    .map(|binding| binding.symbol_id)
            })
    }

    /// Resolve a visible source spelling to the registry key used for its declaration.
    /// Registry maps are keyed by a declaration's original name, while a caller may use
    /// an import alias or a re-export path. Symbol identity bridges those spellings.
    fn visible_decl_name<T>(
        &self,
        current_module: &[String],
        spelling: &str,
        declarations: &HashMap<String, T>,
    ) -> Option<String> {
        if self.symbols.is_empty() {
            return declarations
                .contains_key(spelling)
                .then(|| spelling.to_string());
        }
        let id = self.resolve_type_position_id(current_module, spelling)?;
        self.symbols.iter().find_map(|((_, name), candidate_id)| {
            (*candidate_id == id && declarations.contains_key(name)).then(|| name.clone())
        })
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

    pub fn register_array_method(&mut self, method_name: String, fun_ty: InferType) {
        self.array_method_env.insert(method_name, fun_ty);
    }

    pub fn register_array_method_receiver(
        &mut self,
        method_name: String,
        receiver_kind: ReceiverKind,
    ) {
        self.array_method_receiver_env
            .insert(method_name, receiver_kind);
    }

    pub fn register_struct_type_params(&mut self, name: String, type_params: Vec<TypeVar>) {
        self.struct_type_params.insert(name, type_params);
    }

    pub fn register_struct_generic_names(&mut self, name: String, param_names: Vec<String>) {
        self.struct_generic_names.insert(name, param_names);
    }

    #[must_use]
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

    #[must_use]
    pub fn method_scheme_for(
        &self,
        type_name: &str,
        method_name: &str,
    ) -> Option<&(TypeScheme, Vec<TypeVar>)> {
        self.method_scheme_env.get(type_name)?.get(method_name)
    }

    /// Push a variant method scheme (RFC-0036 §3.1 multi-impl dispatch).
    pub fn register_method_scheme_variant(
        &mut self,
        type_name: String,
        method_name: String,
        scheme: TypeScheme,
        struct_tvars: Vec<TypeVar>,
        aspect_name: Option<String>,
        method_span: Span,
    ) {
        self.generic_method_schemes_by_span
            .insert(method_span, scheme.clone());
        self.method_scheme_variants
            .entry(type_name)
            .or_default()
            .entry(method_name)
            .or_default()
            .push((scheme, struct_tvars, aspect_name));
    }

    pub fn register_array_method_scheme(
        &mut self,
        method_name: String,
        scheme: TypeScheme,
        element_tvars: Vec<TypeVar>,
    ) {
        self.array_method_scheme_env
            .insert(method_name, (scheme, element_tvars));
    }

    #[must_use]
    pub fn array_method_scheme_for(
        &self,
        method_name: &str,
    ) -> Option<&(TypeScheme, Vec<TypeVar>)> {
        self.array_method_scheme_env.get(method_name)
    }

    pub fn register_array_method_scheme_variant(
        &mut self,
        method_name: String,
        scheme: TypeScheme,
        element_tvars: Vec<TypeVar>,
        aspect_name: Option<String>,
        method_span: Span,
    ) {
        self.generic_method_schemes_by_span
            .insert(method_span, scheme.clone());
        self.array_method_scheme_variants
            .entry(method_name)
            .or_default()
            .push((scheme, element_tvars, aspect_name));
    }

    /// Return the exact scheme inferred for one generic method declaration.
    #[must_use]
    pub fn generic_method_scheme_for_decl(&self, method_span: &Span) -> Option<&TypeScheme> {
        self.generic_method_schemes_by_span.get(method_span)
    }

    /// All registered schemes for `method_name` on a structural array target
    /// (issue #272) -- unlike `array_method_scheme_for`'s single slot (last
    /// registration wins), this returns every candidate so a caller can pick
    /// the one whose bounds the concrete element type actually satisfies.
    #[must_use]
    pub fn array_method_scheme_variants_for(
        &self,
        method_name: &str,
    ) -> &[ArrayMethodSchemeVariant] {
        self.array_method_scheme_variants
            .get(method_name)
            .map_or(&[], Vec::as_slice)
    }

    /// All registered schemes for `(type_name, method_name)` on a generic
    /// struct/enum target (issue #272) -- see
    /// `array_method_scheme_variants_for`'s doc for why a caller needs the
    /// full list rather than `method_scheme_for`'s single slot.
    #[must_use]
    pub fn method_scheme_variants_for(
        &self,
        type_name: &str,
        method_name: &str,
    ) -> &[MethodSchemeVariant] {
        self.method_scheme_variants
            .get(type_name)
            .and_then(|m| m.get(method_name))
            .map_or(&[], Vec::as_slice)
    }

    /// Register the conditional impl bounds for a `(target_id, aspect)` key (RFC-0036).
    pub fn register_conditional_impl_bounds(
        &mut self,
        current_module: &[String],
        target: &str,
        aspect: &str,
        pos_bounds: Vec<Vec<GenericBound>>,
        neg_bounds: Vec<Vec<GenericBound>>,
    ) {
        let Some(target_id) = self.resolve_type_position_id(current_module, target) else {
            return;
        };
        self.conditional_impl_bounds
            .entry((target_id, aspect.to_string()))
            .or_default()
            .push((pos_bounds, neg_bounds));
    }

    /// Register a generic negative impl (RFC-0081): one conditional entry keyed by
    /// the target type head. Empty bound vectors represent an unconditional blanket
    /// negative impl such as `impl<T> !Aspect for Foo<T> {}`; non-empty vectors carry
    /// inline/where bounds for the target's type parameters.
    pub fn register_neg_conditional_impl_bounds(
        &mut self,
        current_module: &[String],
        target: &str,
        aspect: &str,
        pos_bounds: Vec<Vec<GenericBound>>,
        neg_bounds: Vec<Vec<GenericBound>>,
    ) {
        let Some(target_id) = self.resolve_type_position_id(current_module, target) else {
            return;
        };
        self.neg_conditional_impl_bounds
            .entry((target_id, aspect.to_string()))
            .or_default()
            .push((pos_bounds, neg_bounds));
    }

    pub fn register_bare_impl_bounds(
        &mut self,
        aspect: &str,
        pos_bounds: Vec<Vec<GenericBound>>,
        neg_bounds: Vec<Vec<GenericBound>>,
    ) {
        self.bare_impl_bounds
            .entry(aspect.to_string())
            .or_default()
            .push((pos_bounds, neg_bounds));
    }

    pub fn register_symbolic_named_aspects(&mut self, name: String, aspects: HashSet<String>) {
        self.symbolic_named_aspects.insert(name, aspects);
    }

    pub fn register_array_impl_bounds(
        &mut self,
        aspect: &str,
        pos_bounds: Vec<Vec<GenericBound>>,
        neg_bounds: Vec<Vec<GenericBound>>,
    ) {
        self.array_impl_bounds
            .entry(aspect.to_string())
            .or_default()
            .push((pos_bounds, neg_bounds));
    }

    pub fn register_neg_bare_impl_bounds(
        &mut self,
        aspect: &str,
        pos_bounds: Vec<Vec<GenericBound>>,
        neg_bounds: Vec<Vec<GenericBound>>,
    ) {
        self.bare_neg_impl_bounds
            .entry(aspect.to_string())
            .or_default()
            .push((pos_bounds, neg_bounds));
    }

    pub fn register_neg_array_impl_bounds(
        &mut self,
        aspect: &str,
        pos_bounds: Vec<Vec<GenericBound>>,
        neg_bounds: Vec<Vec<GenericBound>>,
    ) {
        self.array_neg_impl_bounds
            .entry(aspect.to_string())
            .or_default()
            .push((pos_bounds, neg_bounds));
    }

    /// Register a concrete negative impl (RFC-0060 §5 / issue #244): `impl !Aspect
    /// for Target`. `target_args` is the target's own concrete type-arg list
    /// (e.g. `[i64]` for `impl !Marker for Foo<i64> {}`).
    pub fn register_neg_impl(
        &mut self,
        current_module: &[String],
        target: &str,
        aspect: &str,
        target_args: Vec<Type>,
    ) {
        let Some(target_id) = self.resolve_type_position_id(current_module, target) else {
            return;
        };
        self.neg_impl_env
            .entry((target_id, aspect.to_string()))
            .or_default()
            .push(target_args);
    }

    /// Whether an explicit negative impl exists for this exact concrete instantiation
    /// (RFC-0060 §5 priority: a negative impl overrides a blanket positive impl).
    fn neg_impl_overrides(
        &self,
        target_id: SymbolId,
        aspect_name: &str,
        type_args: &[InferType],
    ) -> bool {
        // Stored concrete args are embedded for the comparison rather than the
        // query args being lowered: a query arg may be a type *variable*, which
        // has no `Type` form. Embedding makes such an arg simply compare
        // unequal, which is the right answer — a negative impl written for one
        // concrete instantiation says nothing about an abstract parameter.
        self.neg_impl_env
            .get(&(target_id, aspect_name.to_string()))
            .is_some_and(|entries| {
                entries.iter().any(|args| {
                    args.len() == type_args.len()
                        && args
                            .iter()
                            .zip(type_args)
                            .all(|(stored, queried)| &type_to_infer(stored) == queried)
                })
            })
    }

    /// Check one conditional impl entry: for each type-argument position, every
    /// positive bound aspect must be satisfied and every negative bound aspect must
    /// not be satisfied.
    fn check_conditional_entry(
        &self,
        current_module: &[String],
        type_args: &[InferType],
        pos_bounds: &[Vec<GenericBound>],
        neg_bounds: &[Vec<GenericBound>],
        assumptions: &AspectAssumptions,
    ) -> bool {
        for (i, arg) in type_args.iter().enumerate() {
            if let Some(required) = pos_bounds.get(i) {
                for aspect in required.iter().filter_map(GenericBound::aspect_name) {
                    if !self.infer_type_satisfies_aspect(current_module, arg, aspect, assumptions) {
                        return false;
                    }
                }
            }
            if let Some(forbidden) = neg_bounds.get(i) {
                for aspect in forbidden.iter().filter_map(GenericBound::aspect_name) {
                    if self.infer_type_satisfies_aspect(current_module, arg, aspect, assumptions) {
                        return false;
                    }
                }
            }
        }
        true
    }

    /// Check whether a concrete `Type` satisfies `aspect_name`, recursing into
    /// nested generic type arguments. Consults `conditional_impl_bounds` (RFC-0036)
    /// in addition to the unconditional `impl_aspect_env`.
    #[must_use]
    pub fn type_satisfies_aspect(
        &self,
        current_module: &[String],
        ty: &Type,
        aspect_name: &str,
    ) -> bool {
        self.infer_type_satisfies_aspect(
            current_module,
            &type_to_infer(ty),
            aspect_name,
            &AspectAssumptions::new(),
        )
    }

    /// The aspect-satisfaction query, over `InferType` and under a set of
    /// assumptions about abstract type parameters.
    ///
    /// Stated over `InferType` rather than `Type` because a type parameter has
    /// to be *representable* for the question to be askable at all. Deciding
    /// `extend<T: Copy> Outer<T>: Copy` for `struct Outer<T> { inner: Inner<T> }`
    /// means asking whether `Inner<T>` is `Copy`, and `Type` cannot hold that
    /// question — its contract is that generics are already monomorphised away
    /// (issue #303). `InferType::Var` is the representation that already exists
    /// for "a type that is not a concrete named type", so the query lives here
    /// and `type_satisfies_aspect` embeds into it.
    ///
    /// A variable answers *only* from `assumptions`, and is never resolved
    /// against a declaration. That is the whole point of keying on `TypeVar`:
    /// a parameter and a same-named struct are different things, and only a
    /// structural distinction keeps them apart. An unbounded parameter has an
    /// empty assumption set and so satisfies nothing, which is the safe
    /// direction — it rejects rather than accepts.
    #[must_use]
    #[allow(clippy::too_many_lines)]
    pub fn infer_type_satisfies_aspect(
        &self,
        current_module: &[String],
        ty: &InferType,
        aspect_name: &str,
        assumptions: &AspectAssumptions,
    ) -> bool {
        if let InferType::Var(var) = ty {
            return assumptions
                .get(var)
                .is_some_and(|assumed| assumed.contains(aspect_name));
        }
        if let InferType::Named(name, args) = ty {
            if args.is_empty()
                && self
                    .symbolic_named_aspects
                    .get(name)
                    .is_some_and(|aspects| aspects.contains(aspect_name))
            {
                return true;
            }
        }
        if let Some(entries) = self.bare_neg_impl_bounds.get(aspect_name) {
            for (pos_bounds, neg_bounds) in entries {
                if self.check_conditional_entry(
                    current_module,
                    std::slice::from_ref(ty),
                    pos_bounds,
                    neg_bounds,
                    assumptions,
                ) {
                    return false;
                }
            }
        }
        if let Some(entries) = self.bare_impl_bounds.get(aspect_name) {
            for (pos_bounds, neg_bounds) in entries {
                if self.check_conditional_entry(
                    current_module,
                    std::slice::from_ref(ty),
                    pos_bounds,
                    neg_bounds,
                    assumptions,
                ) {
                    return true;
                }
            }
        }
        match ty {
            InferType::Record(fields) => {
                if aspect_name == "Send" || aspect_name == "Sync" {
                    return fields.iter().all(|(_, field_ty)| {
                        self.infer_type_satisfies_aspect(
                            current_module,
                            field_ty,
                            aspect_name,
                            assumptions,
                        )
                    });
                }
                false
            }
            InferType::SizedArray(elem, _) => {
                if aspect_name == "Copy" {
                    // #299: fixed-size-array `Copy` stays hardcoded here until const generics
                    // exist and the stdlib can express `[T; N]: Copy` directly.
                    return self.infer_type_satisfies_aspect(
                        current_module,
                        elem,
                        "Copy",
                        assumptions,
                    );
                }
                false
            }
            InferType::Tuple(items) => {
                if aspect_name == "Copy" {
                    // #299: tuple `Copy` stays hardcoded here until tuple impl targets stop
                    // being checker-only and can move into the stdlib.
                    return items.iter().all(|item| {
                        self.infer_type_satisfies_aspect(current_module, item, "Copy", assumptions)
                    });
                }
                false
            }
            InferType::Reference(_) => {
                if aspect_name == "Copy" {
                    return true;
                }
                false
            }
            InferType::MutReference(_) => {
                if aspect_name == "Copy" {
                    return false;
                }
                false
            }
            InferType::Array(elem) => {
                if aspect_name == "Copy" {
                    return true;
                }
                let inner_args = std::slice::from_ref(elem.as_ref());
                if let Some(entries) = self.array_neg_impl_bounds.get(aspect_name) {
                    for (pos_bounds, neg_bounds) in entries {
                        if self.check_conditional_entry(
                            current_module,
                            inner_args,
                            pos_bounds,
                            neg_bounds,
                            assumptions,
                        ) {
                            return false;
                        }
                    }
                }
                if let Some(entries) = self.array_impl_bounds.get(aspect_name) {
                    for (pos_bounds, neg_bounds) in entries {
                        if self.check_conditional_entry(
                            current_module,
                            inner_args,
                            pos_bounds,
                            neg_bounds,
                            assumptions,
                        ) {
                            return true;
                        }
                    }
                }
                false
            }
            InferType::Named(name, inner_args) => {
                let name = name.as_str();
                if let Some(target_id) = self.resolve_type_position_id(current_module, name) {
                    if let Some(entries) = self
                        .neg_conditional_impl_bounds
                        .get(&(target_id, aspect_name.to_string()))
                    {
                        for (pos_bounds, neg_bounds) in entries {
                            if self.check_conditional_entry(
                                current_module,
                                inner_args,
                                pos_bounds,
                                neg_bounds,
                                assumptions,
                            ) {
                                return false;
                            }
                        }
                    }
                    if self.neg_impl_overrides(target_id, aspect_name, inner_args) {
                        return false;
                    }
                }
                if self.impl_aspect_env_has(current_module, name, aspect_name) {
                    return true;
                }
                if let Some(target_id) = self.resolve_type_position_id(current_module, name) {
                    if let Some(entries) = self
                        .conditional_impl_bounds
                        .get(&(target_id, aspect_name.to_string()))
                    {
                        for (pos_bounds, neg_bounds) in entries {
                            if self.check_conditional_entry(
                                current_module,
                                inner_args,
                                pos_bounds,
                                neg_bounds,
                                assumptions,
                            ) {
                                return true;
                            }
                        }
                    }
                }
                false
            }
            // `Var` is answered by the assumption lookup above and cannot
            // reach here; `Never` and `Fun` implement nothing, matching what
            // the pre-`InferType` version returned for them.
            InferType::Var(_) | InferType::Never | InferType::Fun(_, _) => false,
            InferType::Concrete(other) => {
                let name = match other {
                    Type::Str => "String",
                    Type::Boolean => "boolean",
                    Type::Char => "Char",
                    Type::I8 => "i8",
                    Type::I16 => "i16",
                    Type::I32 => "i32",
                    Type::I64 => "i64",
                    Type::U8 => "u8",
                    Type::U16 => "u16",
                    Type::U32 => "u32",
                    Type::U64 => "u64",
                    Type::F32 => "f32",
                    Type::F64 => "f64",
                    _ => return false,
                };
                let Some(target_id) = self.resolve_type_position_id(current_module, name) else {
                    return false;
                };
                if let Some(entries) = self
                    .neg_conditional_impl_bounds
                    .get(&(target_id, aspect_name.to_string()))
                {
                    for (pos_bounds, neg_bounds) in entries {
                        if self.check_conditional_entry(
                            current_module,
                            &[],
                            pos_bounds,
                            neg_bounds,
                            assumptions,
                        ) {
                            return false;
                        }
                    }
                }
                if self.neg_impl_overrides(target_id, aspect_name, &[]) {
                    return false;
                }
                self.impl_aspect_env_has(current_module, name, aspect_name)
            }
        }
    }

    pub fn register_type_param_bounds(&mut self, name: String, bounds: Vec<Vec<GenericBound>>) {
        self.type_param_bounds.insert(name, bounds);
    }

    pub fn register_type_param_record_kinds(&mut self, name: String, record_kinds: Vec<bool>) {
        self.type_param_record_kinds.insert(name, record_kinds);
    }

    #[must_use]
    pub fn type_param_bounds_for(&self, name: &str) -> Option<&Vec<Vec<GenericBound>>> {
        self.type_param_bounds.get(name)
    }

    #[must_use]
    pub fn type_param_record_kinds_for(&self, name: &str) -> Option<&Vec<bool>> {
        self.type_param_record_kinds.get(name)
    }

    pub fn register_neg_type_param_bounds(&mut self, name: String, bounds: Vec<Vec<GenericBound>>) {
        self.neg_type_param_bounds.insert(name, bounds);
    }

    #[must_use]
    pub fn neg_type_param_bounds_for(&self, name: &str) -> Option<&Vec<Vec<GenericBound>>> {
        self.neg_type_param_bounds.get(name)
    }

    /// Returns true if `type_name` has a registered `impl AspectName` in the env.
    /// `type_name` is resolved from `current_module`'s own scope (see
    /// `resolve_type_position_id`); `aspect_name` is matched literally by name — see
    /// `impl_aspect_env`'s doc for why the aspect half stays name-keyed.
    #[must_use]
    pub fn impl_aspect_env_has(
        &self,
        current_module: &[String],
        type_name: &str,
        aspect_name: &str,
    ) -> bool {
        let Some(type_id) = self.resolve_type_position_id(current_module, type_name) else {
            return false;
        };
        self.impl_aspect_env
            .contains_key(&(type_id, aspect_name.to_string()))
    }

    pub fn register_fun_bounds(
        &mut self,
        name: String,
        bounds: HashMap<TypeVar, Vec<GenericBound>>,
    ) {
        if !bounds.is_empty() {
            self.fun_bounds.insert(name, bounds);
        }
    }

    #[must_use]
    pub fn fun_bounds_for(&self, name: &str) -> Option<&HashMap<TypeVar, Vec<GenericBound>>> {
        self.fun_bounds.get(name)
    }

    pub fn register_fun_record_kinds(
        &mut self,
        name: String,
        record_kinds: HashMap<TypeVar, bool>,
    ) {
        if record_kinds.values().any(|flag| *flag) {
            self.fun_record_kinds.insert(name, record_kinds);
        }
    }

    #[must_use]
    pub fn fun_record_kinds_for(&self, name: &str) -> Option<&HashMap<TypeVar, bool>> {
        self.fun_record_kinds.get(name)
    }

    pub fn register_neg_fun_bounds(
        &mut self,
        name: String,
        bounds: HashMap<TypeVar, Vec<GenericBound>>,
    ) {
        if !bounds.is_empty() {
            self.neg_fun_bounds.insert(name, bounds);
        }
    }

    #[must_use]
    pub fn neg_fun_bounds_for(&self, name: &str) -> Option<&HashMap<TypeVar, Vec<GenericBound>>> {
        self.neg_fun_bounds.get(name)
    }

    pub fn register_fun_assoc_eq_constraints(
        &mut self,
        name: String,
        constraints: AssocEqConstraints,
    ) {
        if !constraints.is_empty() {
            self.fun_assoc_eq_constraints.insert(name, constraints);
        }
    }

    #[must_use]
    pub fn fun_assoc_eq_constraints_for(&self, name: &str) -> Option<&AssocEqConstraints> {
        self.fun_assoc_eq_constraints.get(name)
    }

    pub fn register_enum(&mut self, name: String, info: EnumInfo, declaring_module: Vec<String>) {
        for variant in &info.variants {
            let entry = self
                .variant_declaring_enums
                .entry(variant.name.clone())
                .or_default();
            if !entry.contains(&name) {
                entry.push(name.clone());
            }
        }
        self.enum_env.insert(name.clone(), info);
        self.enum_decl_modules.insert(name, declaring_module);
    }

    #[must_use]
    pub fn struct_fields(&self, name: &str) -> Option<&Vec<FieldEntry>> {
        self.struct_env.get(name)
    }

    #[must_use]
    pub fn struct_type_params_for(&self, name: &str) -> Option<&Vec<TypeVar>> {
        self.struct_type_params.get(name)
    }

    #[must_use]
    pub fn method_type(&self, type_name: &str, method_name: &str) -> Option<&InferType> {
        self.method_env.get(type_name)?.get(method_name)
    }

    #[must_use]
    pub fn array_method_type(&self, method_name: &str) -> Option<&InferType> {
        self.array_method_env.get(method_name)
    }

    #[must_use]
    pub fn method_receiver_kind(
        &self,
        type_name: &str,
        method_name: &str,
    ) -> Option<&ReceiverKind> {
        self.method_receiver_env.get(type_name)?.get(method_name)
    }

    #[must_use]
    pub fn array_method_receiver_kind(&self, method_name: &str) -> Option<&ReceiverKind> {
        self.array_method_receiver_env.get(method_name)
    }

    #[must_use]
    pub fn enum_info(&self, name: &str) -> Option<&EnumInfo> {
        self.enum_env.get(name)
    }

    #[must_use]
    pub fn has_variant_named(&self, variant_name: &str) -> bool {
        self.variant_declaring_enums.contains_key(variant_name)
    }

    #[must_use]
    pub fn struct_declaring_module(&self, name: &str) -> Option<&Vec<String>> {
        self.struct_decl_modules.get(name)
    }

    #[must_use]
    pub fn enum_declaring_module(&self, name: &str) -> Option<&Vec<String>> {
        self.enum_decl_modules.get(name)
    }

    pub fn register_aspect(&mut self, name: String, methods: Vec<String>) {
        self.aspect_env.insert(name, methods);
    }

    pub fn register_aspect_generics(&mut self, name: String, generics: Vec<String>) {
        self.aspect_generics.insert(name, generics);
    }

    #[must_use]
    pub fn aspect_generics(&self, name: &str) -> Option<&Vec<String>> {
        self.aspect_generics.get(name)
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
    #[must_use]
    pub fn aspect_declaring_module(&self, name: &str) -> Option<&Vec<String>> {
        self.aspect_decl_modules.get(name)
    }

    /// Whether an aspect name is visible from `current_module`.
    ///
    /// This follows the same type-position lookup as impl targets: unqualified names
    /// use the module's local/import/glob scope, while a qualified path must name the
    /// aspect's declaring module exactly. Keeping that rule in the registry prevents
    /// eager annotation validation from drifting away from later bound resolution.
    #[must_use]
    pub(crate) fn is_visible_aspect(&self, current_module: &[String], name: &str) -> bool {
        self.visible_decl_name(current_module, name, &self.aspect_env)
            .is_some()
    }

    pub fn register_aspect_method_defs(&mut self, name: String, methods: Vec<AspectMethod>) {
        self.aspect_method_defs.insert(name, methods);
    }

    #[must_use]
    pub fn aspect_method_defs(&self, name: &str) -> Option<&Vec<AspectMethod>> {
        self.aspect_method_defs.get(name)
    }

    /// Register the associated-type declarations of an aspect (RFC-0082 §1).
    pub fn register_aspect_assoc_types(&mut self, name: String, decls: Vec<AssocTypeDecl>) {
        if !decls.is_empty() {
            self.aspect_assoc_type_decls.insert(name, decls);
        }
    }

    /// Return the associated-type declarations for `aspect_name`, if any.
    #[must_use]
    pub fn aspect_assoc_type_decls(&self, aspect_name: &str) -> Option<&Vec<AssocTypeDecl>> {
        self.aspect_assoc_type_decls.get(aspect_name)
    }

    /// Register the concrete associated-type bindings for `impl Aspect for Target`
    /// (RFC-0082 §2). `target` is resolved from `current_module`'s scope — same
    /// convention as `register_aspect_impl`.
    pub fn register_impl_assoc_types(
        &mut self,
        current_module: &[String],
        target: &str,
        aspect: &str,
        bindings: HashMap<String, Type>,
    ) {
        let Some(target_id) = self.resolve_type_position_id(current_module, target) else {
            return;
        };
        self.impl_assoc_types
            .entry((target_id, aspect.to_string()))
            .or_default()
            .extend(bindings);
    }

    /// Look up a concrete associated-type binding for a specific impl.
    /// Returns `Some(ty)` if `Target: Aspect` has `type AssocName = ty`.
    #[must_use]
    pub fn impl_assoc_type(
        &self,
        current_module: &[String],
        target: &str,
        aspect: &str,
        assoc_name: &str,
    ) -> Option<&Type> {
        let target_id = self.resolve_type_position_id(current_module, target)?;
        self.impl_assoc_types
            .get(&(target_id, aspect.to_string()))?
            .get(assoc_name)
    }

    /// Registers `impl aspect for target` with `type_args`. `target` is resolved from
    /// `current_module`'s scope to its `SymbolId`; `aspect` stays a literal name (see
    /// `impl_aspect_env`'s doc). A no-op if `target` can't be resolved from that
    /// module's scope — matches this registry's existing graceful-degradation style
    /// elsewhere (e.g. `symbols` being absent entirely on the tolerated no-resolver
    /// path) rather than surfacing an error a caller has no good way to act on; a
    /// genuinely unresolvable target name is caught earlier, by the typechecker's own
    /// name resolution over the impl block itself.
    pub fn register_aspect_impl(
        &mut self,
        current_module: &[String],
        target: &str,
        aspect: &str,
        type_args: Vec<Type>,
    ) {
        let Some(target_id) = self.resolve_type_position_id(current_module, target) else {
            return;
        };
        self.register_aspect_impl_by_id(target_id, aspect, type_args);
    }

    /// Registers `impl aspect for target` with `target` already resolved to an id —
    /// for the handful of hand-registered builtin impls (`Range`/`RangeInclusive`'s
    /// `Iterable`) whose target is a fixed `SYM_TYPE_*` constant, not a name needing
    /// scope resolution.
    pub fn register_aspect_impl_by_id(
        &mut self,
        target: SymbolId,
        aspect: &str,
        type_args: Vec<Type>,
    ) {
        self.impl_aspect_env
            .entry((target, aspect.to_string()))
            .or_default()
            .push(type_args);
    }

    /// Checks `(target, "From")` for an impl with first type-arg matching `source`.
    /// `target` is resolved from `current_module`'s scope; `"From"` is matched
    /// literally — see `impl_aspect_env`'s doc for why the aspect half stays
    /// name-keyed (a module-local `aspect From<T>` must not shadow the builtin one for
    /// this specific bookkeeping).
    #[must_use]
    pub fn has_from_impl(&self, current_module: &[String], target: &str, source: &Type) -> bool {
        let Some(target_id) = self.resolve_type_position_id(current_module, target) else {
            return false;
        };
        self.impl_aspect_env
            .get(&(target_id, "From".to_string()))
            .is_some_and(|impls| impls.iter().any(|args| args.first() == Some(source)))
    }

    /// Returns the element type registered for `(target, "Iterable")`, if any. See
    /// `has_from_impl`'s doc for why the aspect half stays name-keyed.
    #[must_use]
    pub fn iterable_elem_type(&self, current_module: &[String], target: &str) -> Option<&Type> {
        let target_id = self.resolve_type_position_id(current_module, target)?;
        self.impl_aspect_env
            .get(&(target_id, "Iterable".to_string()))
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
    // One independent per-field merge block per registry field; splitting it up
    // would scatter one coherent operation across many small functions with no
    // real gain in clarity.
    #[allow(clippy::too_many_lines)]
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
            // Merge per-method, not per-type: a type may already have some method
            // schemes here (e.g. List's native methods registered into this
            // module's registry) while `other` carries that type's bodied methods
            // (List::map/filter/... checked in std::core). A type-level or_insert
            // would drop the latter entirely.
            let entry = self.method_scheme_env.entry(k.clone()).or_default();
            for (method_name, scheme) in v {
                entry
                    .entry(method_name.clone())
                    .or_insert_with(|| scheme.clone());
            }
        }
        for (k, v) in &other.method_scheme_variants {
            // Concatenate variant lists (cross-module conditional impls for the
            // same method are legitimate).
            let entry = self.method_scheme_variants.entry(k.clone()).or_default();
            for (method_name, variants) in v {
                entry
                    .entry(method_name.clone())
                    .or_default()
                    .extend(variants.iter().cloned());
            }
        }
        for (method_name, scheme) in &other.array_method_scheme_env {
            self.array_method_scheme_env
                .entry(method_name.clone())
                .or_insert_with(|| scheme.clone());
        }
        for (method_name, variants) in &other.array_method_scheme_variants {
            self.array_method_scheme_variants
                .entry(method_name.clone())
                .or_default()
                .extend(variants.iter().cloned());
        }
        for (span, scheme) in &other.generic_method_schemes_by_span {
            self.generic_method_schemes_by_span
                .entry(span.clone())
                .or_insert_with(|| scheme.clone());
        }
        for (k, v) in &other.type_param_bounds {
            self.type_param_bounds
                .entry(k.clone())
                .or_insert_with(|| v.clone());
        }
        for (k, v) in &other.neg_type_param_bounds {
            self.neg_type_param_bounds
                .entry(k.clone())
                .or_insert_with(|| v.clone());
        }
        for (k, v) in &other.fun_bounds {
            self.fun_bounds
                .entry(k.clone())
                .or_insert_with(|| v.clone());
        }
        for (k, v) in &other.neg_fun_bounds {
            self.neg_fun_bounds
                .entry(k.clone())
                .or_insert_with(|| v.clone());
        }
        for (k, v) in &other.fun_assoc_eq_constraints {
            self.fun_assoc_eq_constraints
                .entry(k.clone())
                .or_insert_with(|| v.clone());
        }
        for (k, v) in &other.method_env {
            // Merge per-method, not per-type (see `method_scheme_env` above): two
            // independent modules can each implement a different aspect for the
            // same foreign type (e.g. `impl Shower for Point` in one module and
            // `impl Debugger for Point` in another). A type-level or_insert would
            // silently drop whichever one is merged second.
            let entry = self.method_env.entry(k.clone()).or_default();
            for (method_name, ty) in v {
                entry
                    .entry(method_name.clone())
                    .or_insert_with(|| ty.clone());
            }
        }
        for (k, v) in &other.method_receiver_env {
            let entry = self.method_receiver_env.entry(k.clone()).or_default();
            for (method_name, receiver) in v {
                entry
                    .entry(method_name.clone())
                    .or_insert_with(|| receiver.clone());
            }
        }
        for (method_name, ty) in &other.array_method_env {
            self.array_method_env
                .entry(method_name.clone())
                .or_insert_with(|| ty.clone());
        }
        for (method_name, receiver) in &other.array_method_receiver_env {
            self.array_method_receiver_env
                .entry(method_name.clone())
                .or_insert_with(|| receiver.clone());
        }
        for (k, v) in &other.enum_env {
            self.enum_env.entry(k.clone()).or_insert_with(|| v.clone());
        }
        for (variant_name, enum_names) in &other.variant_declaring_enums {
            let entry = self
                .variant_declaring_enums
                .entry(variant_name.clone())
                .or_default();
            for enum_name in enum_names {
                if !entry.contains(enum_name) {
                    entry.push(enum_name.clone());
                }
            }
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
        for (k, v) in &other.aspect_generics {
            self.aspect_generics
                .entry(k.clone())
                .or_insert_with(|| v.clone());
        }
        for (k, v) in &other.aspect_method_defs {
            self.aspect_method_defs
                .entry(k.clone())
                .or_insert_with(|| v.clone());
        }
        for (name, aspects) in &other.symbolic_named_aspects {
            self.symbolic_named_aspects
                .entry(name.clone())
                .or_insert_with(|| aspects.clone());
        }
        for (k, v) in &other.impl_aspect_env {
            self.impl_aspect_env
                .entry(k.clone())
                .or_insert_with(|| v.clone());
        }
        for (k, v) in &other.conditional_impl_bounds {
            // Concatenate: multiple modules can each declare a conditional impl
            // for the same (target, aspect) pair.
            self.conditional_impl_bounds
                .entry(k.clone())
                .or_default()
                .extend(v.iter().cloned());
        }
        for (k, v) in &other.bare_impl_bounds {
            self.bare_impl_bounds
                .entry(k.clone())
                .or_default()
                .extend(v.iter().cloned());
        }
        for (k, v) in &other.array_impl_bounds {
            self.array_impl_bounds
                .entry(k.clone())
                .or_default()
                .extend(v.iter().cloned());
        }
        for (k, v) in &other.neg_conditional_impl_bounds {
            self.neg_conditional_impl_bounds
                .entry(k.clone())
                .or_default()
                .extend(v.iter().cloned());
        }
        for (k, v) in &other.bare_neg_impl_bounds {
            self.bare_neg_impl_bounds
                .entry(k.clone())
                .or_default()
                .extend(v.iter().cloned());
        }
        for (k, v) in &other.array_neg_impl_bounds {
            self.array_neg_impl_bounds
                .entry(k.clone())
                .or_default()
                .extend(v.iter().cloned());
        }
        for (k, v) in &other.neg_impl_env {
            self.neg_impl_env
                .entry(k.clone())
                .or_default()
                .extend(v.iter().cloned());
        }
        for (k, v) in &other.aspect_assoc_type_decls {
            self.aspect_assoc_type_decls
                .entry(k.clone())
                .or_insert_with(|| v.clone());
        }
        for (k, v) in &other.impl_assoc_types {
            self.impl_assoc_types
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
    loop_depth: usize,
    registry: TypeDefinitionRegistry,
    /// Type-param name → `TypeVar` for the currently-being-inferred generic function.
    /// Empty when inferring a non-generic function or at top level.
    current_type_params: HashMap<String, TypeVar>,
    /// `TypeVar` → aspect names for the current generic function's bounded type params.
    /// Parallel to `current_type_params`; swapped in/out alongside it.
    current_type_param_bounds: HashMap<TypeVar, Vec<GenericBound>>,
    /// Memo + accumulator for symbolic associated-type projections minted while inferring
    /// the CURRENT function/method body. Key: (`base_tv`, `aspect_name`, `assoc_name`) so the
    /// same projection requested twice gets the same placeholder. Reset (swapped, like
    /// `current_type_param_bounds`) on entry/exit of each function/method body.
    current_assoc_projections: AssocProjectionMemo,
    /// Flat log of everything minted above, in insertion order.
    recorded_assoc_projections: AssocProjectionLog,
    current_module_path: Vec<String>,
    /// Names that have same-tier glob conflicts deferred until use. (METEL-98)
    /// Maps name → list of source module paths that both export it.
    deferred_glob_conflicts: HashMap<String, Vec<Vec<String>>>,
    /// `TypeVars` introduced by unsuffixed integer literals (`42`, `1_000`).
    /// Any such var that is still free after constraint solving defaults to `i64`.
    integer_literal_vars: HashSet<TypeVar>,
    /// `TypeVars` introduced by unsuffixed float literals (`3.14`, `2.0`).
    /// Any such var that is still free after constraint solving defaults to `f64`.
    float_literal_vars: HashSet<TypeVar>,
    /// `TypeVars` for opaque return values (RFC-0037). These vars must NOT be bound
    /// to concrete types by the caller - they should remain abstract to enforce opacity.
    opaque_return_vars: HashSet<TypeVar>,
    /// `TypeVar` → the declared generic-parameter name it was minted for (#266),
    /// e.g. the fresh var standing in for `Wrap<T>`'s `T` at one particular
    /// construction site maps to `"T"` here. Consulted by `render_types` so a
    /// diagnostic can show the name the programmer wrote instead of an anonymous
    /// `?1`. Populated only where a fresh var is minted *for* a declared
    /// parameter (struct/enum literal construction so far — see
    /// `tag_declared_var_name`'s callers); an ordinary internal unification
    /// variable that never corresponded to anything in source is never in this
    /// map, and correctly falls back to the message-local placeholder instead.
    declared_var_names: HashMap<TypeVar, String>,
    /// Inferred return type for each closure expression, keyed by the closure span.
    /// Pass 2 reuses this so unannotated closures keep their solved return type.
    closure_return_types: HashMap<Span, InferType>,
    cached_subst: Substitution,
    solved_constraint_count: usize,
    solve_stats: SolveStats,
    /// Free-function overload sets for the current module (METEL-180). Names with
    /// a single definition never appear here. Built by `typechecker::overload`.
    overloads: OverloadTable,
    /// RFC-0111 bare-variant deferrals: (span, variant name, the fresh var standing in
    /// for it). Pass 1 cannot resolve these — only pass 2 knows the expected type — so
    /// each is checked after the final solve. A deferral that never resolved means the
    /// name resolves to nothing at all, which pass 2 will never see because it only ever
    /// runs where an expected type exists. See `unresolved_variant_deferrals`.
    variant_deferrals: Vec<(Span, String, TypeVar)>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SolveStats {
    pub solve_calls: u64,
    pub constraints_processed: u64,
    pub solve_ns: u64,
}

/// One concrete signature within a free-function overload set (METEL-180).
/// Pure data over [`Type`]; the build/selection logic lives in
/// `typechecker::overload`.
#[derive(Debug, Clone)]
pub struct OverloadEntry {
    pub params: Vec<Type>,
    pub ret: Type,
    /// Stable identity for this definition. Typed call sites carry the selected
    /// candidate's id (`TypedExpr::Call::callee_id`) and the evaluator
    /// dispatches through its symbol registry — no name mangling.
    pub symbol_id: crate::symbols::SymbolId,
}

/// Maps a function name to its overload candidates. Only names with more than
/// one free `fun` declaration appear; single-definition functions follow the
/// ordinary name-keyed pipeline unchanged.
pub type OverloadTable = std::collections::HashMap<String, Vec<OverloadEntry>>;

impl InferContext {
    /// Create a new inference context with a pre-built registry, a generator
    /// that has already been advanced past all `TypeVars` allocated during registry
    /// construction (ensuring global `TypeVar` uniqueness), and the set of imported
    /// schemes to seed into the `poly_env`. See ADR-0022.
    #[must_use]
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
            loop_depth: 0,
            registry,
            current_type_params: HashMap::new(),
            current_type_param_bounds: HashMap::new(),
            current_assoc_projections: HashMap::new(),
            recorded_assoc_projections: Vec::new(),
            current_module_path,
            deferred_glob_conflicts: HashMap::new(),
            integer_literal_vars: HashSet::new(),
            float_literal_vars: HashSet::new(),
            declared_var_names: HashMap::new(),
            opaque_return_vars: HashSet::new(),
            closure_return_types: HashMap::new(),
            cached_subst: Substitution::new(),
            solved_constraint_count: 0,
            solve_stats: SolveStats::default(),
            overloads: OverloadTable::new(),
            variant_deferrals: Vec::new(),
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
    #[must_use]
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

    #[must_use]
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

    pub fn register_array_method(&mut self, method_name: String, fun_ty: InferType) {
        self.registry.register_array_method(method_name, fun_ty);
    }

    #[must_use]
    pub fn get_struct_fields(&self, name: &str) -> Option<&Vec<crate::typeinference::FieldEntry>> {
        self.registry.struct_fields(name)
    }

    #[must_use]
    pub fn get_method_type(&self, type_name: &str, method_name: &str) -> Option<&InferType> {
        self.registry.method_type(type_name, method_name)
    }

    #[must_use]
    pub fn get_array_method_type(&self, method_name: &str) -> Option<&InferType> {
        self.registry.array_method_type(method_name)
    }

    #[must_use]
    pub fn get_method_receiver_kind(
        &self,
        type_name: &str,
        method_name: &str,
    ) -> Option<&ReceiverKind> {
        self.registry.method_receiver_kind(type_name, method_name)
    }

    #[must_use]
    pub fn get_array_method_receiver_kind(&self, method_name: &str) -> Option<&ReceiverKind> {
        self.registry.array_method_receiver_kind(method_name)
    }

    pub fn register_enum(&mut self, name: String, info: EnumInfo) {
        self.registry
            .register_enum(name, info, self.current_module_path.clone());
    }

    #[must_use]
    pub fn get_enum(&self, name: &str) -> Option<&EnumInfo> {
        self.registry.enum_info(name)
    }

    #[must_use]
    pub fn aspect_method_defs(&self, name: &str) -> Option<&Vec<AspectMethod>> {
        self.registry.aspect_method_defs(name)
    }

    #[must_use]
    pub fn has_from_impl(&self, target: &str, source: &Type) -> bool {
        self.registry
            .has_from_impl(&self.current_module_path, target, source)
    }

    #[must_use]
    pub fn iterable_elem_type(&self, target: &str) -> Option<&Type> {
        self.registry
            .iterable_elem_type(&self.current_module_path, target)
    }

    #[must_use]
    pub fn registry(&self) -> &TypeDefinitionRegistry {
        &self.registry
    }

    #[must_use]
    pub fn current_module_path(&self) -> &[String] {
        &self.current_module_path
    }

    /// Consume the context and return its registry. Used by `check_graph` to extract
    /// accumulated type definitions after a module is checked. See ADR-0032.
    #[must_use]
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
        bounds: HashMap<TypeVar, Vec<GenericBound>>,
    ) -> HashMap<TypeVar, Vec<GenericBound>> {
        std::mem::replace(&mut self.current_type_param_bounds, bounds)
    }

    #[must_use]
    pub fn type_params(&self) -> &HashMap<String, TypeVar> {
        &self.current_type_params
    }

    /// Returns the aspect names required by a type-param `TypeVar` in the current
    /// function scope. Bounds are tracked out-of-band from the types themselves,
    /// so after unification the active representative may differ from the `TypeVar`
    /// the bounds were originally registered on. Merge bounds across the solved
    /// equivalence class rooted at the cached substitution's representative.
    #[must_use]
    pub fn bounds_for_type_var(&self, tv: TypeVar) -> Option<Vec<GenericBound>> {
        let resolved = match self.cached_subst.apply(&InferType::Var(tv)) {
            InferType::Var(v) => v,
            _ => tv,
        };
        let mut merged = self
            .current_type_param_bounds
            .get(&tv)
            .cloned()
            .unwrap_or_default();
        if resolved != tv {
            if let Some(bounds) = self.current_type_param_bounds.get(&resolved) {
                for bound in bounds {
                    if !merged.iter().any(|existing| match (existing, bound) {
                        (GenericBound::Aspect(left), GenericBound::Aspect(right)) => left == right,
                        _ => false,
                    }) {
                        merged.push(bound.clone());
                    }
                }
            }
        }
        for (candidate, bounds) in &self.current_type_param_bounds {
            if *candidate == tv || *candidate == resolved {
                continue;
            }
            let candidate_resolved = match self.cached_subst.apply(&InferType::Var(*candidate)) {
                InferType::Var(v) => v,
                _ => *candidate,
            };
            if candidate_resolved != resolved {
                continue;
            }
            for bound in bounds {
                if !merged.iter().any(|existing| match (existing, bound) {
                    (GenericBound::Aspect(left), GenericBound::Aspect(right)) => left == right,
                    _ => false,
                }) {
                    merged.push(bound.clone());
                }
            }
        }
        if merged.is_empty() {
            None
        } else {
            Some(merged)
        }
    }

    /// Register an aspect bound for a type variable (for opaque return values).
    pub fn register_type_var_bound(&mut self, tv: TypeVar, aspect: String) {
        self.current_type_param_bounds
            .entry(tv)
            .or_default()
            .push(GenericBound::Aspect(aspect));
    }

    /// Mark a type variable as an opaque return that should not be bound to concrete types.
    pub fn mark_opaque_return_var(&mut self, tv: TypeVar) {
        self.opaque_return_vars.insert(tv);
    }

    /// Read-only view of all type-param bounds in the current scope (for debug assertions).
    #[must_use]
    #[allow(dead_code)]
    pub fn type_param_bounds(&self) -> &HashMap<TypeVar, Vec<GenericBound>> {
        &self.current_type_param_bounds
    }

    /// Swap in empty projection state for a new function/method body, returning the old state.
    pub fn swap_assoc_projections(&mut self) -> (AssocProjectionMemo, AssocProjectionLog) {
        let old_memo = std::mem::take(&mut self.current_assoc_projections);
        let old_log = std::mem::take(&mut self.recorded_assoc_projections);
        (old_memo, old_log)
    }

    /// Restore previously-saved projection state (call when leaving a function/method body).
    pub fn restore_assoc_projections(
        &mut self,
        memo: AssocProjectionMemo,
        log: AssocProjectionLog,
    ) {
        self.current_assoc_projections = memo;
        self.recorded_assoc_projections = log;
    }

    /// Mint a fresh `TypeVar` for the projection `T::AssocName` where `T` is `base_tv`
    /// and the method is declared in `aspect_name`. Reuses the same placeholder if the
    /// exact same projection was already minted in the current body (memoized).
    ///
    /// The associated type's own declared bound (`type AssocName: Bound;`, RFC-0082
    /// §1) is registered on the fresh placeholder so that a method call chained
    /// directly onto the projection result (e.g. `c.get().to_string()` where `fun
    /// get(&self) -> Item` and `type Item: Display;`) can resolve the receiver's
    /// bound the same way an ordinary bounded generic parameter's would.
    pub fn fresh_assoc_projection_var(
        &mut self,
        base_tv: TypeVar,
        aspect_name: &str,
        assoc_name: &str,
    ) -> TypeVar {
        let key = (base_tv, aspect_name.to_string(), assoc_name.to_string());
        if let Some(&existing) = self.current_assoc_projections.get(&key) {
            return existing;
        }
        let placeholder = self.var_gen.fresh();
        self.current_assoc_projections
            .insert(key.clone(), placeholder);
        self.recorded_assoc_projections
            .push((key.0, key.1, key.2, placeholder));

        let declared_bounds: Vec<String> = self
            .registry
            .aspect_assoc_type_decls(aspect_name)
            .into_iter()
            .flatten()
            .filter(|decl| decl.name == assoc_name)
            .flat_map(|decl| &decl.bounds)
            .filter(|b| b.polarity == crate::ast::Polarity::Positive)
            .filter_map(|b| b.aspect_name().map(ToOwned::to_owned))
            .collect();
        for bound in declared_bounds {
            self.register_type_var_bound(placeholder, bound);
        }

        placeholder
    }

    /// Drain the accumulated projection log. Call after `solve()` to build the scheme's
    /// `assoc_projections` mapping.
    pub fn take_recorded_assoc_projections(&mut self) -> AssocProjectionLog {
        std::mem::take(&mut self.recorded_assoc_projections)
    }

    /// Returns the aspect method defs from the registry.
    #[must_use]
    pub fn get_aspect_method_defs(&self, aspect: &str) -> Option<&Vec<crate::ast::AspectMethod>> {
        self.registry.aspect_method_defs(aspect)
    }

    pub fn register_fun_bounds(
        &mut self,
        name: String,
        bounds: HashMap<TypeVar, Vec<GenericBound>>,
    ) {
        self.registry.register_fun_bounds(name, bounds);
    }

    pub fn register_fun_record_kinds(
        &mut self,
        name: String,
        record_kinds: HashMap<TypeVar, bool>,
    ) {
        self.registry.register_fun_record_kinds(name, record_kinds);
    }

    pub fn register_neg_fun_bounds(
        &mut self,
        name: String,
        bounds: HashMap<TypeVar, Vec<GenericBound>>,
    ) {
        self.registry.register_neg_fun_bounds(name, bounds);
    }

    pub fn register_fun_assoc_eq_constraints(
        &mut self,
        name: String,
        constraints: AssocEqConstraints,
    ) {
        self.registry
            .register_fun_assoc_eq_constraints(name, constraints);
    }

    #[must_use]
    pub fn struct_generic_names_for(&self, name: &str) -> Option<&Vec<String>> {
        self.registry.struct_generic_names_for(name)
    }

    #[must_use]
    pub fn get_type_param_bounds(&self, name: &str) -> Option<&Vec<Vec<GenericBound>>> {
        self.registry.type_param_bounds_for(name)
    }

    #[must_use]
    pub fn get_type_param_record_kinds(&self, name: &str) -> Option<&Vec<bool>> {
        self.registry.type_param_record_kinds_for(name)
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

    pub fn register_array_method_scheme(
        &mut self,
        method_name: String,
        scheme: TypeScheme,
        element_tvars: Vec<TypeVar>,
    ) {
        self.registry
            .register_array_method_scheme(method_name, scheme, element_tvars);
    }

    pub fn register_method_scheme_variant(
        &mut self,
        type_name: String,
        method_name: String,
        scheme: TypeScheme,
        struct_tvars: Vec<TypeVar>,
        aspect_name: Option<String>,
        method_span: Span,
    ) {
        self.registry.register_method_scheme_variant(
            type_name,
            method_name,
            scheme,
            struct_tvars,
            aspect_name,
            method_span,
        );
    }

    pub fn register_array_method_scheme_variant(
        &mut self,
        method_name: String,
        scheme: TypeScheme,
        element_tvars: Vec<TypeVar>,
        aspect_name: Option<String>,
        method_span: Span,
    ) {
        self.registry.register_array_method_scheme_variant(
            method_name,
            scheme,
            element_tvars,
            aspect_name,
            method_span,
        );
    }

    #[must_use]
    pub fn method_scheme_for(
        &self,
        type_name: &str,
        method_name: &str,
    ) -> Option<&(TypeScheme, Vec<TypeVar>)> {
        self.registry.method_scheme_for(type_name, method_name)
    }

    #[must_use]
    pub fn array_method_scheme_for(
        &self,
        method_name: &str,
    ) -> Option<&(TypeScheme, Vec<TypeVar>)> {
        self.registry.array_method_scheme_for(method_name)
    }

    /// Return a new generator whose counter starts immediately past all vars
    /// allocated by this context.  Use this to hand off to a subsequent phase
    /// (Pass 2, `register_builtin_poly_schemes`) so that every `TypeVar` ever
    /// produced during a type-check run is globally unique.
    #[must_use]
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
    ///
    /// # Panics
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

    /// Create a fresh `TypeVar` for an unsuffixed integer literal.
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

    /// Create a fresh `TypeVar` for an unsuffixed float literal.
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

    /// Extend `subst` so that any literal `TypeVar` still free (unbound to a concrete type)
    /// is defaulted: integer literal vars → `i64`, float literal vars → `f64`.
    /// Also propagates defaults through `TypeVar` chains: if a literal var resolves to
    /// another free `TypeVar`, both are bound to the default type.
    /// Call this immediately after each `ctx.solve()` before using the substitution.
    #[must_use]
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

    /// Record that `var` was minted for the declared generic parameter `name`
    /// (#266) — e.g. a fresh var standing in for `Wrap<T>`'s `T` at one
    /// particular struct-literal construction site. Overwrites any previous
    /// tag for `var`, matching `Substitution::bind`'s own last-write semantics;
    /// in practice each var is tagged at most once, right where it is minted.
    pub fn tag_declared_var_name(&mut self, var: TypeVar, name: String) {
        self.declared_var_names.insert(var, name);
    }

    /// Instantiate a scheme and retain the source-to-fresh variable mapping for
    /// callers that must subsequently pin or annotate a particular parameter.
    /// Declared, non-empty parameter names follow their fresh variables so
    /// diagnostics can render those variables using source-level names.
    pub fn instantiate_with_renaming(
        &mut self,
        scheme: &TypeScheme,
    ) -> (InferType, HashMap<TypeVar, TypeVar>) {
        let (instance, renaming) = instantiate_with_renaming(scheme, &mut self.var_gen);
        for (&original, name) in scheme.quantified_vars.iter().zip(&scheme.param_names) {
            if !name.is_empty() {
                if let Some(&fresh) = renaming.get(&original) {
                    self.tag_declared_var_name(fresh, name.clone());
                }
            }
        }
        (instance, renaming)
    }

    /// Instantiate a scheme when the caller has no need for its renaming map.
    pub fn instantiate(&mut self, scheme: &TypeScheme) -> InferType {
        self.instantiate_with_renaming(scheme).0
    }

    /// Bind a name to a monomorphic type in the current scope.
    /// `is_mutable` is `true` for `mut` bindings, `false` for `let` bindings and parameters.
    ///
    /// # Panics
    /// Panics if called with no scope pushed — cannot happen through normal use,
    /// since a fresh `InferContext` always starts with one scope.
    pub fn bind_mono(&mut self, name: impl Into<String>, ty: InferType, is_mutable: bool) {
        self.mono_env
            .last_mut()
            .unwrap()
            .insert(name.into(), (ty, is_mutable));
    }

    /// Install the module's free-function overload table (METEL-180).
    pub fn set_overloads(&mut self, overloads: OverloadTable) {
        self.overloads = overloads;
    }

    /// Whether `name` has more than one free-function definition in this module.
    #[must_use]
    pub fn is_overloaded(&self, name: &str) -> bool {
        self.overloads.contains_key(name)
    }

    /// The overload candidates for `name`, or `None` if it is not overloaded.
    #[must_use]
    pub fn overload_candidates(&self, name: &str) -> Option<&[OverloadEntry]> {
        self.overloads.get(name).map(std::vec::Vec::as_slice)
    }

    /// Bind a name to a polymorphic type scheme in the current scope.
    ///
    /// # Panics
    /// Panics if called with no scope pushed — see [`InferContext::bind_mono`].
    pub fn bind_poly(&mut self, name: impl Into<String>, scheme: TypeScheme) {
        self.poly_env
            .last_mut()
            .unwrap()
            .insert(name.into(), scheme);
    }

    /// Bind a polymorphic scheme only if the current scope does not already
    /// contain that name. Used for lower-priority prelude names.
    ///
    /// # Panics
    /// Panics if called with no scope pushed — see [`InferContext::bind_mono`].
    pub fn bind_poly_if_absent(&mut self, name: impl Into<String>, scheme: TypeScheme) {
        self.poly_env
            .last_mut()
            .unwrap()
            .entry(name.into())
            .or_insert(scheme);
    }

    /// Whether any scope binds `name` (poly or mono), without instantiating.
    /// Used by overload resolution to decide if a failed exact-match can fall
    /// back to a non-overload binding (e.g. the `std::core` generic `print`).
    #[must_use]
    pub fn has_binding(&self, name: &str) -> bool {
        self.poly_env.iter().any(|sc| sc.contains_key(name))
            || self.mono_env.iter().any(|sc| sc.contains_key(name))
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
            Some(self.instantiate(&scheme))
        } else {
            self.mono_env
                .iter()
                .rev()
                .find_map(|scope| scope.get(name))
                .map(|(ty, _)| ty.clone())
        }
    }

    /// Look up a polymorphic scheme by name without instantiation.
    /// Used for checking opaque return metadata without instantiating.
    #[must_use]
    pub fn poly_scheme(&self, name: &str) -> Option<TypeScheme> {
        self.poly_env
            .iter()
            .rev()
            .find_map(|scope| scope.get(name))
            .cloned()
    }

    /// Look up a name's type regardless of its mutability, or `None` if it isn't bound.
    /// Used by RFC-0067a's write-through rule: a non-`mut` binding of type `&mut T` may
    /// still be written through (the exclusivity comes from the reference, not the
    /// binding), so `lookup_for_write`'s immutability check must be bypassed to inspect
    /// the raw type before deciding whether that applies. (RFC-0110 retired the
    /// write-through rule this was introduced for; kept for its other callers.)
    /// Record a bare identifier deferred by RFC-0111's gate, so an unresolvable one can
    /// be reported after solving rather than silently accepted.
    pub fn record_variant_deferral(&mut self, span: Span, name: String, var: TypeVar) {
        self.variant_deferrals.push((span, name, var));
    }

    /// Bare-variant deferrals that never resolved to a concrete enum. Reported after the
    /// final solve. A resolved deferral is one whose stand-in variable became a named
    /// type — pass 2 then resolves the variant against it. One that is still a variable,
    /// or became `!`, is a name that resolves to nothing: reachable only where no
    /// expected type ever arrived, which RFC-0111 §1.4 defines as an error.
    #[must_use]
    pub fn unresolved_variant_deferrals(&self, subst: &Substitution) -> Vec<(Span, String)> {
        self.variant_deferrals
            .iter()
            .filter(|(_, _, var)| {
                !matches!(
                    subst.apply(&InferType::Var(*var)),
                    InferType::Named(..) | InferType::Concrete(_)
                )
            })
            .map(|(span, name, _)| (span.clone(), name.clone()))
            .collect()
    }

    #[must_use]
    pub fn lookup_mono_raw(&self, name: &str) -> Option<InferType> {
        self.mono_env
            .iter()
            .rev()
            .find_map(|scope| scope.get(name))
            .map(|(ty, _)| ty.clone())
    }

    /// Look up a name for writing (assignment). Returns the binding's type on success.
    ///
    /// # Errors
    /// - T0003 if the name is not in scope
    /// - T0006 if the binding is immutable (`let` or parameter)
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
    #[must_use]
    pub fn env_free_vars(&self) -> HashSet<TypeVar> {
        let mut vars = HashSet::new();
        for scope in &self.mono_env {
            for (ty, _) in scope.values() {
                collect_free_vars(ty, &mut vars);
            }
        }
        vars
    }

    /// Record that `lhs` and `rhs` must unify, tagged with its source location.
    pub fn add_constraint(&mut self, lhs: InferType, rhs: InferType, span: Span) {
        self.constraints.push(Constraint::new(lhs, rhs, span));
    }

    /// Record an operand-agreement constraint, tagged with the operator so a failure can
    /// say which one it was instead of only which two types disagreed.
    pub fn add_operand_constraint(
        &mut self,
        lhs: InferType,
        rhs: InferType,
        span: Span,
        op: &'static str,
    ) {
        self.constraints
            .push(Constraint::for_operator(lhs, rhs, span, op));
    }

    /// Solve all accumulated constraints and return the resulting substitution.
    ///
    /// # Errors
    /// Returns an error if any accumulated constraint fails to unify.
    pub fn solve(&mut self) -> Result<Substitution, MetelError> {
        let started = Instant::now();
        self.solve_stats.solve_calls += 1;

        let mut subst = self.cached_subst.clone();
        for constraint in &self.constraints[self.solved_constraint_count..] {
            apply_constraint_with_coercion(
                &mut subst,
                constraint,
                &self.integer_literal_vars,
                &self.float_literal_vars,
                &self.opaque_return_vars,
                &self.registry,
                &self.declared_var_names,
            )?;
        }

        self.solve_stats.constraints_processed +=
            (self.constraints.len() - self.solved_constraint_count) as u64;
        self.solve_stats.solve_ns += started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64;
        self.solved_constraint_count = self.constraints.len();
        self.cached_subst = subst.clone();

        Ok(subst)
    }

    #[must_use]
    pub fn solve_stats(&self) -> SolveStats {
        self.solve_stats
    }

    pub fn record_closure_return_type(&mut self, span: Span, ty: InferType) {
        self.closure_return_types.insert(span, ty);
    }

    #[must_use]
    pub fn closure_return_types(&self) -> &HashMap<Span, InferType> {
        &self.closure_return_types
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
    #[must_use]
    pub fn current_return_type(&self) -> Option<&InferType> {
        self.current_return_type.as_ref()
    }

    pub fn push_break_type(&mut self, ty: InferType) -> Option<InferType> {
        self.current_break_type.replace(ty)
    }

    pub fn pop_break_type(&mut self, prev: Option<InferType>) {
        self.current_break_type = prev;
    }

    #[must_use]
    pub fn current_break_type(&self) -> Option<&InferType> {
        self.current_break_type.as_ref()
    }

    pub fn enter_loop(&mut self) {
        self.loop_depth += 1;
    }

    pub fn exit_loop(&mut self) {
        debug_assert!(self.loop_depth > 0, "loop depth underflow");
        self.loop_depth -= 1;
    }

    #[must_use]
    pub fn push_loop_depth_reset(&mut self) -> usize {
        std::mem::replace(&mut self.loop_depth, 0)
    }

    pub fn pop_loop_depth(&mut self, prev: usize) {
        self.loop_depth = prev;
    }

    #[must_use]
    pub fn is_in_loop(&self) -> bool {
        self.loop_depth > 0
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
