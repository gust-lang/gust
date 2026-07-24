use crate::parser::Rule;
use crate::symbols::SymbolId;

// ── Span ──────────────────────────────────────────────────────────────────────

/// Source location (byte offsets + resolved line/col into the original source string).
///
/// `Eq`/`Hash` are derived so a `Span` can key the reference-resolution side table
/// (see `reference_resolver`): each reference site has a unique `(start, end, filename)`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub filename: String,
    pub line: u32,
    pub col: u32,
}

impl Span {
    pub fn new(start: usize, end: usize, filename: impl Into<String>) -> Self {
        Self {
            start,
            end,
            filename: filename.into(),
            line: 0,
            col: 0,
        }
    }
}

impl Span {
    pub fn of(pair: &pest::iterators::Pair<Rule>, filename: impl Into<String>) -> Self {
        let s = pair.as_span();
        let (line, col) = s.start_pos().line_col();
        Span {
            start: s.start(),
            end: s.end(),
            filename: filename.into(),
            line: line as u32,
            col: col as u32,
        }
    }
}

// ── Top-level ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Program {
    #[allow(dead_code)]
    pub imports: Vec<ImportDecl>,
    #[allow(dead_code)]
    pub exports: Vec<ExportDecl>,
    pub decls: Vec<Decl>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Visibility {
    Private,
    Public,
}

#[derive(Debug, Clone)]
pub struct ImportDecl {
    #[allow(dead_code)]
    pub path: ImportPath,
    #[allow(dead_code)]
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct ExportDecl {
    #[allow(dead_code)]
    pub path: ImportPath,
    #[allow(dead_code)]
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportPath {
    pub root: PathRoot,
    pub tree: ImportTree,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathRoot {
    Root,
    Std,
    Self_,
    Super,
    Name(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportTree {
    Name { name: String, alias: Option<String> },
    Group(Vec<ImportTree>),
    Glob,
    Path { name: String, tree: Box<ImportTree> },
}

// ── Declarations ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum Decl {
    Let(LetDecl),
    Mut(MutDecl),
    Fun(FunDecl),
    Struct(StructDecl),
    Enum(EnumDecl),
    Impl(ImplBlock),
    Aspect(AspectDecl),
    Stmt(Box<Stmt>),
}

#[derive(Debug, Clone)]
pub struct LetDecl {
    pub name: String,
    pub type_ann: Option<TypeExpr>,
    pub value: Expr,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct MutDecl {
    pub name: String,
    pub type_ann: Option<TypeExpr>,
    pub value: Expr,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct FunDecl {
    #[allow(dead_code)] // read by name_resolver; not yet wired into the typechecker pipeline
    pub visibility: Visibility,
    pub name: String,
    pub generics: Vec<GenericParam>,
    pub where_clause: Option<WhereClause>,
    pub params: Vec<Param>,
    pub return_type: Option<TypeExpr>,
    /// Host binding for stdlib-only `native(@…)` functions (METEL-182). When
    /// present, `body` is an empty placeholder and the function dispatches to a
    /// host implementation keyed by the lowered [`NativeKey`].
    pub native: Option<NativeBinding>,
    pub body: Block,
    pub span: Span,
}

/// A `native(@std.core.print)` host-binding attribute on a stdlib function.
#[derive(Debug, Clone)]
pub struct NativeBinding {
    /// Dotted surface id segments, e.g. `["std", "core", "print"]`.
    pub key_path: Vec<String>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct StructDecl {
    #[allow(dead_code)]
    pub visibility: Visibility,
    pub name: String,
    pub generics: Vec<GenericParam>,
    pub where_clause: Option<WhereClause>,
    pub fields: Vec<FieldDef>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct EnumDecl {
    #[allow(dead_code)]
    pub visibility: Visibility,
    pub name: String,
    pub generics: Vec<GenericParam>,
    pub where_clause: Option<WhereClause>,
    pub variants: Vec<VariantDef>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct ImplBlock {
    /// `Negative` for `impl !Aspect for Type {}` (RFC-0081) — body must be empty,
    /// checked by the parser. Not yet coherence-checked (issue #264's job); this
    /// field exists so the syntax parses and `registry.rs` doesn't register a
    /// negative impl as a positive one.
    pub polarity: Polarity,
    /// Type parameters scoped to this impl block (RFC-0036), e.g. `impl<T: Bound>
    /// Aspect for Type<T> { ... }`. Empty for a non-generic impl.
    pub generics: Vec<GenericParam>,
    pub aspect_name: Option<String>,
    pub aspect_type_args: Vec<TypeExpr>,
    pub target_type: TypeExpr,
    /// The `where T: Bound` form of RFC-0036's conditional impls, equivalent to an
    /// inline bound in `generics`. Not yet consumed — real bound-satisfaction
    /// checking at each instantiation is issue #241's job.
    #[allow(dead_code)]
    pub where_clause: Option<WhereClause>,
    /// `type Name = ConcreteType;` definitions (RFC-0082). Not yet checked against
    /// the aspect's own declared associated types (issue #242's job) — this only
    /// makes the syntax parse and carry through to the typed AST.
    #[allow(dead_code)]
    pub assoc_type_defs: Vec<AssocTypeDef>,
    pub methods: Vec<FunDecl>,
    pub span: Span,
}

/// `type Name = ConcreteType;` inside an `impl` block (RFC-0082 SS2).
#[derive(Debug, Clone)]
pub struct AssocTypeDef {
    pub name: String,
    pub ty: TypeExpr,
    #[allow(dead_code)] // not yet consumed; will back diagnostics once needed
    pub span: Span,
}

/// `type Name;` / `type Name: Bound;` inside an `aspect` block (RFC-0082 SS1).
#[derive(Debug, Clone)]
pub struct AssocTypeDecl {
    pub name: String,
    pub bounds: Vec<Bound>,
    #[allow(dead_code)] // not yet consumed; will back diagnostics once needed
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct AspectDecl {
    #[allow(dead_code)]
    pub visibility: Visibility,
    pub name: String,
    pub generics: Vec<String>,
    /// `type Name;` / `type Name: Bound;` member declarations (RFC-0082 SS1).
    /// Enforced against impl definitions (issue #242).
    pub assoc_types: Vec<AssocTypeDecl>,
    pub methods: Vec<AspectMethod>,
    pub span: Span,
}

// ── Supporting types ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Polarity {
    Positive,
    Negative,
}

/// A single bound in a bound-list position (`GenericParam.bounds`,
/// `WhereClause.constraints`'s values). `assoc_bindings` is separate, additive
/// storage for RFC-0082's equality constraints (`Deref<Target = Node>`) — these
/// aren't ordinary recursive `TypeExpr` args and must not leak into `named_type`'s
/// general instantiation grammar (see `grammar.pest`'s `bound_head`/`bound_arg`).
#[derive(Debug, Clone)]
pub struct Bound {
    pub polarity: Polarity,
    pub aspect: TypeExpr,
    pub assoc_bindings: Vec<(String, TypeExpr)>,
    #[allow(dead_code)] // not yet consumed; will back diagnostics once bounds are checked
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct GenericParam {
    pub name: String,
    pub bounds: Vec<Bound>, // empty = unconstrained
}

#[derive(Debug, Clone)]
pub struct WhereClause {
    pub constraints: Vec<(String, Vec<Bound>)>, // (type_param_name, [bound, ...])
}

#[derive(Debug, Clone)]
pub enum ReceiverKind {
    Value,
    Ref,
    RefMut,
}

#[derive(Debug, Clone)]
pub struct Param {
    pub mutable: bool,
    pub receiver: Option<ReceiverKind>,
    pub name: String,
    pub type_ann: Option<TypeExpr>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct FieldDef {
    pub visibility: Visibility,
    pub name: String,
    pub type_ann: TypeExpr,
    /// Reserved for span propagation through the type registry (v0.4.3, #133).
    #[allow(dead_code)]
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct VariantDef {
    pub name: String,
    pub fields: Vec<FieldDef>,
    /// Reserved for span propagation through the type registry (v0.4.3, #133).
    #[allow(dead_code)]
    pub span: Span,
}

/// An aspect method declaration. Fields beyond `name` are reserved for
/// aspect completeness checking and default body dispatch (not yet implemented).
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct AspectMethod {
    pub name: String,
    pub generics: Vec<GenericParam>,
    pub params: Vec<Param>,
    pub return_type: Option<TypeExpr>,
    pub default_body: Option<Block>,
    pub span: Span,
}

// ── Block ─────────────────────────────────────────────────────────────────────

/// `{ decl* expr? }` — the `tail` expression is the block's value when used in
/// expression position (if-expr, loop-expr, closure body, etc.).
#[derive(Debug, Clone)]
pub struct Block {
    pub stmts: Vec<Decl>,
    pub tail: Option<Box<Expr>>,
    pub span: Span,
}

// ── Statements ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum Stmt {
    While(WhileStmt),
    For(Box<ForStmt>),
    ForIn(Box<ForInStmt>),
    Expr(Expr),
}

#[derive(Debug, Clone)]
pub struct WhileStmt {
    pub condition: Expr,
    pub body: Block,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct ForStmt {
    pub init: Option<ForInit>,
    pub condition: Option<Expr>,
    pub step: Option<Expr>,
    pub body: Block,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum ForInit {
    Let(LetDecl),
    Mut(MutDecl),
    Expr(Expr),
}

#[derive(Debug, Clone)]
pub struct ForInStmt {
    pub binding: String,
    pub mutable: bool,
    pub iterable: Expr,
    pub body: Block,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct ReturnExpr {
    pub value: Option<Box<Expr>>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct BreakExpr {
    pub value: Option<Box<Expr>>,
    pub span: Span,
}

// ── Expressions ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum Expr {
    Literal(Literal, Span),
    Ident(String, Span),
    Path(Vec<String>, Span),
    /// Produced by the path normalizer (#185). A multi-segment `Expr::Path` that
    /// has been resolved to a single bare name. `resolved` is the name the
    /// typechecker uses for lookup; `original` is retained for error messages.
    /// Produced by the path normalizer: a multi-segment module-qualified path rewritten to its
    /// local alias. `resolved` is the local name used for lookup; `symbol_id` is the stable
    /// identity of the underlying declaration (None for keyword-root paths like `root::` or
    /// `self::`, and for glob-resolved names without an explicit binding).
    /// `original` preserves the full source spelling for diagnostics.
    ResolvedPath {
        resolved: String,
        symbol_id: Option<SymbolId>,
        original: Vec<String>,
        span: Span,
    },
    Tuple(Vec<Expr>, Span),
    Array(Vec<Expr>, Span),
    RecordLiteral {
        fields: Vec<(String, Expr)>,
        span: Span,
    },
    RepeatArray(Box<Expr>, u64, Span),
    BinOp(Box<Expr>, BinOp, Box<Expr>, Span),
    UnaryOp(UnaryOp, Box<Expr>, Span),
    Assign {
        target: AssignTarget,
        op: AssignOp,
        value: Box<Expr>,
        span: Span,
    },
    Call {
        callee: Box<Expr>,
        type_args: Vec<TypeExpr>,
        args: Vec<Expr>,
        span: Span,
    },
    MethodCall {
        receiver: Box<Expr>,
        method: String,
        type_args: Vec<TypeExpr>,
        args: Vec<Expr>,
        span: Span,
    },
    FieldAccess {
        object: Box<Expr>,
        field: String,
        span: Span,
    },
    TupleAccess {
        object: Box<Expr>,
        index: usize,
        span: Span,
    },
    Index {
        object: Box<Expr>,
        index: Box<Expr>,
        span: Span,
    },
    Cast {
        expr: Box<Expr>,
        target_type: TypeExpr,
        span: Span,
    },
    Ascribe {
        expr: Box<Expr>,
        ann: TypeExpr,
        span: Span,
    },
    Match(MatchExpr),
    If {
        condition: Box<Expr>,
        then_branch: Block,
        else_branch: Option<Block>,
        span: Span,
    },
    Loop {
        body: Block,
        span: Span,
    },
    Closure {
        params: Vec<Param>,
        return_type: Option<TypeExpr>,
        body: Block,
        span: Span,
    },
    StructLiteral {
        path: Vec<String>,
        fields: Vec<(String, Expr)>,
        /// Stable identity of the constructed struct/enum type, resolved by the path
        /// normalizer for module-qualified literals (METEL-185 / ADR-0041). Lets
        /// construction stamp the correct per-reference type id onto the value instead
        /// of re-deriving it from the name-keyed registry (which collides for
        /// same-named cross-module types). `None` for local/unqualified literals,
        /// which resolve correctly via the declaring-module index.
        symbol_id: Option<SymbolId>,
        span: Span,
    },
    RecordProjection {
        path: Vec<String>,
        fields: Vec<String>,
        span: Span,
    },
    PropagateError {
        expr: Box<Expr>,
        span: Span,
    },
    /// Issue #229: `return`/`break`/`continue` are expressions of type `!`
    /// (RFC-0078 bottom-type subtyping/coercion), reachable anywhere an
    /// expression is valid, not just inside a braced statement.
    Return(ReturnExpr),
    Break(BreakExpr),
    Continue(Span),
}

impl Expr {
    #[must_use]
    pub fn span(&self) -> &Span {
        match self {
            Expr::Literal(_, s)
            | Expr::Ident(_, s)
            | Expr::Path(_, s)
            | Expr::ResolvedPath { span: s, .. }
            | Expr::Tuple(_, s)
            | Expr::Array(_, s)
            | Expr::RecordLiteral { span: s, .. }
            | Expr::RepeatArray(_, _, s)
            | Expr::BinOp(_, _, _, s)
            | Expr::UnaryOp(_, _, s)
            | Expr::Assign { span: s, .. }
            | Expr::Call { span: s, .. }
            | Expr::MethodCall { span: s, .. }
            | Expr::FieldAccess { span: s, .. }
            | Expr::TupleAccess { span: s, .. }
            | Expr::Index { span: s, .. }
            | Expr::Cast { span: s, .. }
            | Expr::Ascribe { span: s, .. }
            | Expr::If { span: s, .. }
            | Expr::Loop { span: s, .. }
            | Expr::Closure { span: s, .. }
            | Expr::StructLiteral { span: s, .. }
            | Expr::RecordProjection { span: s, .. }
            | Expr::PropagateError { span: s, .. }
            | Expr::Continue(s) => s,
            Expr::Return(r) => &r.span,
            Expr::Break(b) => &b.span,
            Expr::Match(m) => &m.span,
        }
    }
}

// ── Match ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct MatchExpr {
    pub scrutinee: Box<Expr>,
    pub arms: Vec<MatchArm>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct MatchArm {
    pub pattern: Pattern,
    pub guard: Option<Expr>,
    pub body: Block,
    pub span: Span,
}

// ── Patterns ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum Pattern {
    Wildcard(Span),
    Literal(Literal, Span),
    Binding(String, Span),
    EnumVariant {
        path: Vec<String>,
        fields: Vec<String>,
        span: Span,
    },
    Record {
        fields: Vec<String>,
        span: Span,
    },
    Tuple(Vec<Pattern>, Span),
    Array {
        elems: Vec<Pattern>,
        rest: Option<String>,
        span: Span,
    },
}

// ── Operators ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
    Range,
    RangeInclusive,
}

impl BinOp {
    /// The operator as written in source, for diagnostics.
    #[must_use]
    pub fn symbol(&self) -> &'static str {
        match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Rem => "%",
            BinOp::Eq => "==",
            BinOp::Ne => "!=",
            BinOp::Lt => "<",
            BinOp::Le => "<=",
            BinOp::Gt => ">",
            BinOp::Ge => ">=",
            BinOp::And => "and",
            BinOp::Or => "or",
            BinOp::Range => "..",
            BinOp::RangeInclusive => "..=",
        }
    }
}


#[derive(Debug, Clone, PartialEq)]
pub enum UnaryOp {
    Neg,
    Not,
    Ref,
    RefMut,
    Deref,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AssignOp {
    Assign,
    AddAssign,
    SubAssign,
    MulAssign,
    DivAssign,
    RemAssign,
}

// ── Assignment targets ────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum AssignTarget {
    Ident(String, Span),
    FieldAccess {
        object: Box<Expr>,
        field: String,
        span: Span,
    },
    TupleAccess {
        object: Box<Expr>,
        index: usize,
        span: Span,
    },
    Index {
        object: Box<Expr>,
        index: Box<Expr>,
        span: Span,
    },
    /// RFC-0110: `*p = v` — write through an explicit dereference. The only spelling
    /// that writes through a bare reference-typed binding, now that plain `p = v`
    /// rebinds instead.
    Deref {
        object: Box<Expr>,
        span: Span,
    },
}

// ── Types ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum TypeExpr {
    Named(String, Vec<TypeExpr>),
    Unit,
    Tuple(Vec<TypeExpr>),
    Record(Vec<(String, TypeExpr)>),
    Array(Box<TypeExpr>),
    SizedArray(Box<TypeExpr>, u64),
    Reference(Box<TypeExpr>),
    MutReference(Box<TypeExpr>),
    Fun(Vec<TypeExpr>, Option<Box<TypeExpr>>),
    /// `impl Aspect` in parameter position. Lowered to a fresh anonymous type param before
    /// inference. Retained in the AST only until the lowering pass runs.
    ImplAspect {
        bound: Box<TypeExpr>,
        // Reserved for aspect-related error messages (e.g. "expected `impl Display`") — not yet surfaced.
        #[allow(dead_code)]
        source_spell: String,
        #[allow(dead_code)]
        span: Span,
    },
    /// `T::AssocType` (RFC-0082 SS3) — a projection of a generic parameter's
    /// associated type. `base` is the generic parameter (e.g. `Named("T", [])`);
    /// produced by a post-parse lowering pass (`lower_projections`), not the parser
    /// itself, since recognizing this requires knowing which names are declared
    /// generics — the parser has no such context. Real resolution to a concrete
    /// type (or the ambiguity check for two same-named associated types) is issue
    /// #242's job; this variant exists so the syntax parses and threads through.
    Projection {
        base: Box<TypeExpr>,
        assoc_name: String,
        #[allow(dead_code)] // not yet consumed; will back diagnostics once resolved
        span: Span,
    },
    RecordProjection {
        path: Vec<String>,
        fields: Vec<String>,
        span: Span,
    },
}

// ── Literals ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum IntKind {
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FloatKind {
    F32,
    F64,
}

#[derive(Debug, Clone)]
pub enum Literal {
    Int(i64),
    Float(f64),
    /// An integer literal with an explicit bit-width suffix, e.g. `42u8`, `100i32`.
    /// `value` is stored as `i128` to accommodate the full u64 range.
    SizedInt {
        value: i128,
        kind: IntKind,
    },
    /// A float literal with an explicit precision suffix, e.g. `3.14f32`.
    SizedFloat {
        value: f64,
        kind: FloatKind,
    },
    Char(char),
    Boolean(bool),
    Str(String),
    Unit,
}

// ── String unescaping ─────────────────────────────────────────────────────────
