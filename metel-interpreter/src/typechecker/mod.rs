use std::collections::{HashMap, HashSet};
use std::time::Instant;

use serde::Serialize;

use crate::ast::{Decl, Program, Visibility};
use crate::error::MetelError;
use crate::error::TypeErrorCode;
use crate::module_loader::LoadedModule;
use crate::name_resolver::{GlobTier, ResolvedNames};
use crate::path_normalizer::NormalizedModuleGraph;
use crate::symbols::SymbolId;
use crate::typed_ast::{ResolvedImportRef, TypedDecl, TypedModule, TypedModuleGraph, TypedProgram};
use crate::typeinference::*;

mod construction;
mod conversions;
mod inference;
mod overload;
mod registry;

type SchemeEnv = HashMap<String, TypeScheme>;
type DeferredGlobConflicts = HashMap<String, Vec<Vec<String>>>;

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct TypecheckPhaseTimings {
    pub registry_ns: u64,
    pub inference_ns: u64,
    pub solve_ns: u64,
    pub scheme_env_ns: u64,
    pub construction_ns: u64,
    pub finalize_ns: u64,
    pub solve_calls: u64,
    pub constraints_processed: u64,
}

#[derive(Debug, Clone)]
struct CheckImplReport {
    typed_decls: Vec<TypedDecl>,
    scheme_env: SchemeEnv,
    registry: TypeDefinitionRegistry,
    timings: TypecheckPhaseTimings,
}

#[allow(dead_code)] // public profiling API for benchmark workflows
#[derive(Debug, Clone)]
pub struct CheckWithCtxReport {
    pub decls: TypedProgram,
    pub type_ctx: crate::typeinference::TypeCtx,
    pub timings: TypecheckPhaseTimings,
}

#[allow(dead_code)] // public profiling API for benchmark workflows
#[derive(Debug)]
pub struct CheckGraphReport {
    pub graph: TypedModuleGraph,
    pub timings: TypecheckPhaseTimings,
}

// ── ScopedEnv ─────────────────────────────────────────────────────────────────

/// A single resolved import binding, tracking the source module for conflict
/// reporting. Used by `ScopedEnv` and by #177 (T0011 conflict detection).
#[allow(dead_code)]
enum Binding {
    /// Unambiguous: one scheme from one source module.
    Single {
        scheme: TypeScheme,
        source: ModulePath,
    },
    /// Conflicting glob imports both export the same name.
    /// Deferred error: T0011 fires when the name is looked up.
    Conflict { sources: Vec<ModulePath> },
}

/// Per-module import scope, seeded imports-first then local declarations.
/// Used to build the `SchemeEnv` passed to `check_impl`. (#177 will use this.)
#[allow(dead_code)]
type ScopedEnv = HashMap<String, Binding>;

struct FunGeneralization {
    name: String,
    fun_ty: InferType,
    env_fvs: HashSet<TypeVar>,
    /// Maps TypeVar ID → source-level generic param name, for scheme param_names.
    name_map: HashMap<TypeVar, String>,
}

// ── StdPrelude ────────────────────────────────────────────────────────────────

/// Pre-loaded standard library type schemes, seeded into GlobalExports before
/// the per-module typechecking loop begins.  `StdPrelude` is the single source
/// of truth for all builtin function signatures — `register_builtins` and
/// the construction pass both derive from it.
pub struct StdPrelude {
    schemes: SchemeEnv,
}

impl StdPrelude {
    /// No standard library names pre-loaded. Use in tests that do not need std.
    #[allow(dead_code)] // public API used by module-loading test harness
    pub fn empty() -> Self {
        Self {
            schemes: HashMap::new(),
        }
    }

    pub(super) fn schemes(&self) -> &SchemeEnv {
        &self.schemes
    }

    pub(super) fn contains(&self, name: &str) -> bool {
        self.schemes.contains_key(name)
    }
}

impl Default for StdPrelude {
    /// All built-in function schemes (print, assert, List::new, …).
    /// This is the canonical list — nothing else should register builtins.
    ///
    /// The generator starts at 10000 so that prelude TypeVars never collide
    /// with the registry TypeVars allocated by `build_registry` (which starts
    /// at 0 and typically allocates fewer than 100 vars). See ADR-0027.
    fn default() -> Self {
        let mut schemes = HashMap::new();
        let mut gen = TypeVarGenerator::with_counter(10000);
        registry::populate_std_schemes(&mut schemes, &mut gen);
        Self { schemes }
    }
}

// ── GlobalExports ─────────────────────────────────────────────────────────────

type ModulePath = Vec<String>;

struct ModuleExports {
    pub_schemes: SchemeEnv,
}

struct GlobalExports {
    modules: HashMap<ModulePath, ModuleExports>,
}

impl GlobalExports {
    fn new() -> Self {
        Self {
            modules: HashMap::new(),
        }
    }

    fn insert(&mut self, path: ModulePath, exports: ModuleExports) {
        self.modules.insert(path, exports);
    }

    fn get_scheme(&self, module_path: &[String], name: &str) -> Option<&TypeScheme> {
        self.modules.get(module_path)?.pub_schemes.get(name)
    }

    fn all_pub_schemes(&self, module_path: &[String]) -> Option<&SchemeEnv> {
        Some(&self.modules.get(module_path)?.pub_schemes)
    }
}

// ── check_pub_annotations ─────────────────────────────────────────────────────

/// Enforce T0010: every `pub` function must have explicit return type and
/// explicit parameter type annotations. Runs before inference so errors are
/// surfaced early with clear messages rather than cryptic inference failures
/// when downstream modules attempt to import the function.
fn check_pub_annotations(loaded: &LoadedModule, names: &ResolvedNames) -> Result<(), MetelError> {
    let pub_surface = match names.pub_surface.get(&loaded.module_path) {
        Some(s) => s,
        None => return Ok(()),
    };

    for decl in &loaded.program.decls {
        match decl {
            Decl::Fun(fd) if fd.visibility == Visibility::Public => {
                if !pub_surface.contains(fd.name.as_str()) {
                    continue;
                }
                if fd.return_type.is_none() {
                    return Err(MetelError::type_error(
                        TypeErrorCode::T0010,
                        format!(
                            "public declaration `{}` requires an explicit return type annotation; \
                             add `-> <Type>` after the parameter list",
                            fd.name
                        ),
                        &fd.span,
                    ));
                }
                for param in &fd.params {
                    if param.type_ann.is_none() {
                        return Err(MetelError::type_error(
                            TypeErrorCode::T0010,
                            format!(
                                "public declaration `{}` requires explicit type annotations on \
                                 all parameters; add `: <Type>` to parameter `{}`",
                                fd.name, param.name
                            ),
                            &param.span,
                        ));
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

// ── check_graph ───────────────────────────────────────────────────────────────

/// Typecheck a normalized module graph. Processes modules in topological order
/// (dependencies before dependents); each module is typechecked against its
/// declared imports, with results accumulated into `GlobalExports`.
/// Typecheck a normalized module graph. See ADR-0022 for the GlobalExports accumulator
/// pattern and the invariant that imported_schemes must reach both inference and construction.
pub fn check_graph(
    graph: NormalizedModuleGraph,
    names: &ResolvedNames,
    std_prelude: StdPrelude,
) -> Result<TypedModuleGraph, MetelError> {
    Ok(check_graph_with_report(graph, names, std_prelude)?.graph)
}

pub fn check_graph_with_report(
    graph: NormalizedModuleGraph,
    names: &ResolvedNames,
    std_prelude: StdPrelude,
) -> Result<CheckGraphReport, MetelError> {
    let mut global_exports = GlobalExports::new();

    // Seed std::core into GlobalExports so that std:: imports and the auto-glob resolve.
    // StdPrelude is the single source of truth for all built-in schemes. See ADR-0027.
    global_exports.insert(
        vec!["std".to_string(), "core".to_string()],
        ModuleExports {
            pub_schemes: std_prelude.schemes().clone(),
        },
    );

    let mut typed_modules: Vec<TypedModule> = Vec::new();
    // Accumulated resolved type definitions from already-checked modules.
    // Passed to check_impl so cross-module struct/enum field references are visible.
    // See ADR-0032.
    let mut type_registry = TypeDefinitionRegistry::new();
    let mut timings = TypecheckPhaseTimings::default();

    for loaded in graph.modules() {
        check_pub_annotations(loaded, names)?;
        let (imported_schemes, deferred_conflicts) =
            build_import_schemes(loaded, names, &global_exports, &graph)?;
        let report = check_impl_with_report(
            &loaded.program,
            &imported_schemes,
            deferred_conflicts,
            &type_registry,
            &std_prelude,
            &loaded.module_path,
            Some(&names.symbols),
        )?;
        accumulate_typecheck_timings(&mut timings, report.timings);
        type_registry = report.registry;

        // Export pub names from this module's scheme_env, plus re-exported names
        // pulled from their source modules in GlobalExports (#178).
        let pub_schemes = filter_pub_schemes(&report.scheme_env, loaded, names, &global_exports);
        global_exports.insert(loaded.module_path.clone(), ModuleExports { pub_schemes });

        // Populate imported_names: local_name → (source_module, canonical_name).
        // Used by evaluate_graph to seed each module's isolated Environment. See ADR-0029.
        let (import_aliases, imported_names) = names
            .scopes
            .get(&loaded.module_path)
            .map(|scope| {
                let aliases = scope
                    .explicit
                    .iter()
                    .filter(|(local, binding)| *local != &binding.source_name)
                    .map(|(local, binding)| (local.clone(), binding.source_name.clone()))
                    .collect();

                let mut imports: HashMap<String, ResolvedImportRef> = HashMap::new();

                // Glob imports (lower priority — added first so explicit can override).
                // Process Std then User, mirroring build_import_schemes tier ordering.
                // std::core names are always registered via builtins, so skipping the
                // Std glob here is safe — but we still process User globs for cross-module names.
                let ordered_globs = scope
                    .globs
                    .iter()
                    .filter(|(t, _)| *t == GlobTier::Std)
                    .chain(scope.globs.iter().filter(|(t, _)| *t == GlobTier::User));
                for (_, glob_module) in ordered_globs {
                    let Some(pub_schemes) = global_exports.all_pub_schemes(glob_module) else {
                        continue;
                    };
                    for name in pub_schemes.keys() {
                        imports.insert(
                            name.clone(),
                            ResolvedImportRef {
                                source_module: glob_module.clone(),
                                canonical_name: name.clone(),
                                symbol_id: None,
                            },
                        );
                    }
                }

                // Explicit imports (higher priority — overwrite globs).
                for (local, binding) in &scope.explicit {
                    if binding.kind == crate::name_resolver::BindingKind::Item {
                        imports.insert(
                            local.clone(),
                            ResolvedImportRef {
                                source_module: binding.source_module.clone(),
                                canonical_name: binding.source_name.clone(),
                                symbol_id: Some(binding.symbol_id),
                            },
                        );
                    }
                }

                (aliases, imports)
            })
            .unwrap_or_default();

        // Add builtin schemes so construction-at-call-time can resolve builtins
        // like `array_len` inside generic function bodies.
        let mut full_scheme_env = report.scheme_env;
        registry::register_builtin_schemes(&mut full_scheme_env, &std_prelude);
        typed_modules.push(TypedModule {
            module_path: loaded.module_path.clone(),
            decls: report.typed_decls,
            import_aliases,
            imported_names,
            scheme_env: full_scheme_env,
        });
    }

    Ok(CheckGraphReport {
        graph: TypedModuleGraph {
            modules: typed_modules,
            type_registry,
        },
        timings,
    })
}

/// Build the set of imported name→scheme bindings for a module, drawn from
/// GlobalExports. Explicit imports take precedence over glob imports.
///
/// For explicit imports: if the name is absent from GlobalExports, checks
/// `names.declared_names` to distinguish T0009 (private item — declared but
/// not pub) from T0003 (name does not exist). See #191.
/// Returns the resolved import schemes plus a map of deferred same-tier glob conflicts.
/// Conflicts are not rejected here; T0011 fires at the use site. (METEL-98)
fn build_import_schemes(
    loaded: &LoadedModule,
    names: &ResolvedNames,
    global_exports: &GlobalExports,
    graph: &NormalizedModuleGraph,
) -> Result<(SchemeEnv, DeferredGlobConflicts), MetelError> {
    let mut env: SchemeEnv = HashMap::new();
    let mut deferred_conflicts: HashMap<String, Vec<Vec<String>>> = HashMap::new();
    let Some(scope) = names.scopes.get(&loaded.module_path) else {
        return Ok((env, deferred_conflicts));
    };

    // Glob imports (lower priority — added first so explicit can override).
    // Process Std globs before User globs so User silently wins cross-tier conflicts.
    // T0011 fires only when two globs of the **same** tier export the same name. See ADR-0026.
    let mut glob_source: HashMap<String, (Vec<String>, GlobTier)> = HashMap::new();
    let ordered_globs = scope
        .globs
        .iter()
        .filter(|(t, _)| *t == GlobTier::Std)
        .chain(scope.globs.iter().filter(|(t, _)| *t == GlobTier::User));
    for (tier, glob_module) in ordered_globs {
        let Some(all_schemes) = global_exports.all_pub_schemes(glob_module) else {
            continue;
        };
        for (name, scheme) in all_schemes {
            let conflict = glob_source
                .get(name.as_str())
                .map(|(s, t)| (s.clone(), t.clone()));
            match conflict {
                Some((prior_source, ref prior_tier)) if prior_tier == tier => {
                    // Same-tier conflict — defer T0011 to the use site. (METEL-98)
                    // Remove the name from env so a use site that sees None gets our error,
                    // not a spurious T0003.
                    env.remove(name.as_str());
                    let entry = deferred_conflicts.entry(name.clone()).or_default();
                    if !entry.contains(&prior_source) {
                        entry.push(prior_source.clone());
                    }
                    if !entry.contains(&glob_module.to_vec()) {
                        entry.push(glob_module.clone());
                    }
                    continue;
                }
                Some((_, GlobTier::User)) => {
                    // Prior User glob claimed this name; current Std glob cannot override.
                }
                _ => {
                    // Either no prior claim, or current User tier overrides a prior Std.
                    glob_source.insert(name.clone(), (glob_module.clone(), tier.clone()));
                    env.insert(name.clone(), scheme.clone());
                }
            }
        }
    }

    // Explicit imports (higher priority — overwrite globs).
    for (local_name, binding) in &scope.explicit {
        if let Some(scheme) =
            global_exports.get_scheme(&binding.source_module, &binding.source_name)
        {
            env.insert(local_name.clone(), scheme.clone());
        } else {
            // No function scheme — check if it is a public struct/enum/aspect (type-only import).
            let is_pub_type = names
                .pub_surface
                .get(&binding.source_module)
                .is_some_and(|surface| surface.contains(binding.source_name.as_str()));
            if is_pub_type {
                // Valid public struct/enum/aspect import — no scheme needed; type registry
                // handles these via type_context. Skip silently.
                continue;
            }
            // Check if the source module is in the graph (not std, which is not file-loaded).
            let src_in_graph = graph
                .modules()
                .iter()
                .any(|m| m.module_path == binding.source_module);
            if src_in_graph {
                let span = find_import_span(loaded, &binding.source_module, &binding.source_name);
                let name_exists = names
                    .declared_names
                    .get(&binding.source_module)
                    .is_some_and(|s| s.contains(binding.source_name.as_str()));
                if name_exists {
                    return Err(MetelError::type_error(
                        TypeErrorCode::T0009,
                        format!(
                            "visibility error: `{}` is not public in module `{}`",
                            binding.source_name,
                            binding.source_module.join("::")
                        ),
                        &span,
                    ));
                } else {
                    return Err(MetelError::type_error(
                        TypeErrorCode::T0003,
                        format!(
                            "cannot import `{}` from module `{}`: name does not exist",
                            binding.source_name,
                            binding.source_module.join("::")
                        ),
                        &span,
                    ));
                }
            }
            // Source not in graph (std or future external crate) — skip silently.
        }
    }

    Ok((env, deferred_conflicts))
}

/// Find the span of the import declaration in `loaded` that references `source_name`
/// from `source_module`. Falls back to a file-level span if no match is found.
fn find_import_span(
    loaded: &LoadedModule,
    source_module: &[String],
    source_name: &str,
) -> crate::ast::Span {
    use crate::ast::{ImportTree, PathRoot};

    fn tree_contains(tree: &ImportTree, name: &str) -> bool {
        match tree {
            ImportTree::Name { name: n, alias } => n == name || alias.as_deref() == Some(name),
            ImportTree::Path { tree, .. } => tree_contains(tree, name),
            ImportTree::Group(items) => items.iter().any(|t| tree_contains(t, name)),
            ImportTree::Glob => false,
        }
    }

    for import in &loaded.program.imports {
        let root_matches = match &import.path.root {
            PathRoot::Name(n) => source_module.first().map(|s| s == n).unwrap_or(false),
            PathRoot::Self_ => source_module == loaded.module_path,
            PathRoot::Root | PathRoot::Super => true,
            PathRoot::Std => false,
        };
        if root_matches && tree_contains(&import.path.tree, source_name) {
            return import.span.clone();
        }
    }
    crate::ast::Span::new(0, 0, loaded.file_path.display().to_string())
}

/// Build the public scheme export for a module: pub-declared names from its
/// own scheme_env, plus any re-exported names pulled from `global_exports`.
fn filter_pub_schemes(
    scheme_env: &SchemeEnv,
    loaded: &LoadedModule,
    names: &ResolvedNames,
    global_exports: &GlobalExports,
) -> SchemeEnv {
    let Some(pub_names) = names.pub_surface.get(&loaded.module_path) else {
        return HashMap::new();
    };

    // Locally-declared pub names from this module's inference output.
    let mut result: SchemeEnv = scheme_env
        .iter()
        .filter(|(name, _)| pub_names.contains(name.as_str()))
        .map(|(name, scheme)| (name.clone(), scheme.clone()))
        .collect();

    // Re-exported names: present in pub_surface but not in scheme_env.
    // Pull their schemes from the source module's GlobalExports entry.
    if let Some(scope) = names.scopes.get(&loaded.module_path) {
        for (local_name, binding) in &scope.re_exports {
            if pub_names.contains(local_name.as_str()) && !result.contains_key(local_name) {
                if let Some(scheme) =
                    global_exports.get_scheme(&binding.source_module, &binding.source_name)
                {
                    result.insert(local_name.clone(), scheme.clone());
                }
            }
        }
    }

    result
}

/// Run the type checker over an untyped AST, producing a fully typed AST.
/// `native` declarations may appear only in standard-library modules (those
/// whose path begins with `std`). Reject them anywhere else (METEL-182).
fn enforce_native_stdlib_only(
    program: &Program,
    module_path: &[String],
) -> Result<(), MetelError> {
    if module_path.first().map(String::as_str) == Some("std") {
        return Ok(());
    }
    fn check_fun(fun: &crate::ast::FunDecl) -> Result<(), MetelError> {
        match &fun.native {
            Some(binding) => Err(MetelError::type_error(
                TypeErrorCode::T0003,
                "`native` functions are only allowed in standard-library modules",
                &binding.span,
            )),
            None => Ok(()),
        }
    }
    for decl in &program.decls {
        match decl {
            crate::ast::Decl::Fun(fun) => check_fun(fun)?,
            crate::ast::Decl::Impl(ib) => {
                for m in &ib.methods {
                    check_fun(m)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

#[allow(dead_code)] // public API used by single-file test harness
pub fn check(program: Program) -> Result<TypedProgram, MetelError> {
    let report = check_impl_with_report(
        &program,
        &HashMap::new(),
        HashMap::new(),
        &TypeDefinitionRegistry::new(),
        &StdPrelude::default(),
        &[],
        None,
    )?;
    Ok(report.typed_decls)
}

/// Run the type checker and also return the type context needed for
/// construction-at-call-time of generic function bodies.
#[allow(dead_code)] // public API used by single-file test harness
pub fn check_with_ctx(
    program: Program,
) -> Result<(TypedProgram, crate::typeinference::TypeCtx), MetelError> {
    let report = check_with_ctx_with_report(program)?;
    Ok((report.decls, report.type_ctx))
}

pub fn check_with_ctx_with_report(program: Program) -> Result<CheckWithCtxReport, MetelError> {
    let std_prelude = StdPrelude::default();
    let report = check_impl_with_report(
        &program,
        &HashMap::new(),
        HashMap::new(),
        &TypeDefinitionRegistry::new(),
        &std_prelude,
        &[],
        None,
    )?;
    let mut full_scheme_env = report.scheme_env;
    registry::register_builtin_schemes(&mut full_scheme_env, &std_prelude);
    Ok(CheckWithCtxReport {
        decls: report.typed_decls,
        type_ctx: crate::typeinference::TypeCtx {
            scheme_env: full_scheme_env,
            registry: report.registry,
        },
        timings: report.timings,
    })
}

/// Construct a `TypedBlock` for a generic (polymorphic) function body at call time.
///
/// Called by the evaluator when it encounters `ClosureBody::Untyped` with a `type_ctx`.
/// Instantiates the function's `TypeScheme` using the runtime argument types, builds
/// a `ConstructCtx`, and runs the typechecker's construction pass on the raw block.
pub(crate) fn construct_generic_body(
    scheme: &TypeScheme,
    params: &[crate::ast::Param],
    arg_types: &[crate::types::Type],
    body: &crate::ast::Block,
    span: &crate::ast::Span,
    type_ctx: &crate::typeinference::TypeCtx,
) -> Result<crate::typed_ast::TypedBlock, MetelError> {
    construction::construct_generic_body(scheme, params, arg_types, body, span, type_ctx)
}

/// Core typechecking pipeline.
///
/// - `imported_schemes`: type schemes from imported modules, seeded into the
///   inference context so imported names are visible.
/// - `base_registry`: resolved type definitions accumulated from already-checked
///   dependency modules. Merged into the freshly-built registry so that cross-module
///   type references in struct fields and method signatures are visible. See ADR-0032.
///
/// Returns `(typed_decls, scheme_env, registry)` where `registry` carries this
/// module's type definitions merged with the base, for the next module to use.
#[allow(dead_code)] // retained as a tuple-returning internal helper for existing call patterns
fn check_impl(
    program: &Program,
    imported_schemes: &SchemeEnv,
    deferred_conflicts: HashMap<String, Vec<Vec<String>>>,
    base_registry: &TypeDefinitionRegistry,
    std_prelude: &StdPrelude,
    current_module_path: &[String],
    symbols: Option<&HashMap<(Vec<String>, String), SymbolId>>,
) -> Result<(Vec<TypedDecl>, SchemeEnv, TypeDefinitionRegistry), MetelError> {
    let report = check_impl_with_report(
        program,
        imported_schemes,
        deferred_conflicts,
        base_registry,
        std_prelude,
        current_module_path,
        symbols,
    )?;
    Ok((report.typed_decls, report.scheme_env, report.registry))
}

fn check_impl_with_report(
    program: &Program,
    imported_schemes: &SchemeEnv,
    deferred_conflicts: HashMap<String, Vec<Vec<String>>>,
    base_registry: &TypeDefinitionRegistry,
    std_prelude: &StdPrelude,
    current_module_path: &[String],
    symbols: Option<&HashMap<(Vec<String>, String), SymbolId>>,
) -> Result<CheckImplReport, MetelError> {
    // `native` declarations are stdlib-only: reject them outside `std::…`.
    enforce_native_stdlib_only(program, current_module_path)?;

    // Lowering pass: desugar `impl Aspect` params to fresh anonymous type params.
    let program = inference::lower_impl_aspects_in_program(program.clone());
    let program = &program;

    let started = Instant::now();
    let mut gen = TypeVarGenerator::new();
    let mut reg = registry::build_registry(program, &mut gen, current_module_path);
    // Merge dependency type definitions so cross-module struct/enum refs resolve.
    reg.merge_from(base_registry);
    let mut ctx = InferContext::new(reg, gen, imported_schemes, current_module_path.to_vec());
    ctx.seed_glob_conflicts(deferred_conflicts);

    // Pre-pass: register built-in value bindings, build the overload table, and
    // hoist function names. The overload table must be installed before hoisting
    // so hoisting can skip overloaded names (they are handled by mangled names).
    registry::register_primitive_type_bindings(&mut ctx, std_prelude);
    let overloads = overload::build_overload_table(&program.decls)?;
    ctx.set_overloads(overloads.clone());
    inference::hoist_fun_decls(&program.decls, &mut ctx);
    let registry_ns = elapsed_ns(started);

    // Pass 1: walk AST, emit constraints, collect function generalizations.
    let started = Instant::now();
    let mut fun_generalizations: Vec<FunGeneralization> = vec![];
    inference::infer_program(program, &mut ctx, &mut fun_generalizations)?;
    let infer_total_ns = elapsed_ns(started);
    let solve_after_inference = ctx.solve_stats();

    let started = Instant::now();
    let solved = ctx.solve()?;
    let final_solve_ns = elapsed_ns(started);
    let subst = ctx.default_literal_vars(&solved);
    let solve_stats = ctx.solve_stats();
    let solve_ns = solve_after_inference.solve_ns + final_solve_ns;
    let inference_ns = infer_total_ns.saturating_sub(solve_after_inference.solve_ns);

    // Build SchemeEnv from user functions, then add all built-in schemes.
    let started = Instant::now();
    let gen = ctx.split_gen();
    let mut scheme_env: SchemeEnv = HashMap::new();
    for fg in fun_generalizations {
        // fg.fun_ty is already post-inline-solve (resolved_ty from infer_fun_decl).
        // Applying the final module-level subst would collapse generic TypeVars that
        // happened to appear in other functions' constraints. (METEL-137)
        let scheme = generalize_with_names(fg.fun_ty, &fg.env_fvs, &fg.name_map);
        scheme_env.insert(fg.name, scheme);
    }
    // Imported schemes must be visible in the construction pass so calls to imported
    // functions can be constructed. Use or_insert so locally-defined names shadow imports.
    // INVARIANT: imported_schemes must be seeded into BOTH InferContext (above, via
    // bind_poly) AND scheme_env (here). Missing either breaks one of the two passes.
    // See ADR-0022.
    for (name, scheme) in imported_schemes {
        scheme_env
            .entry(name.clone())
            .or_insert_with(|| scheme.clone());
    }
    registry::register_builtin_schemes(&mut scheme_env, std_prelude);
    let scheme_env_ns = elapsed_ns(started);

    // Pass 2: construct typed AST for the current module only.
    // The registry owns all type definitions; ConstructCtx derives concrete envs from it.
    let started = Instant::now();
    let typed_decls = construction::construct_program(
        program,
        &subst,
        &scheme_env,
        ctx.registry(),
        gen,
        symbols,
        &overloads,
    )?;
    let construction_ns = elapsed_ns(started);

    // Return only user-defined names. Builtins (from StdPrelude) are available to
    // every module via the auto-glob and don't need to be in GlobalExports.
    let started = Instant::now();
    let local_value_names = top_level_value_names(program);
    let user_scheme_env: SchemeEnv = scheme_env
        .into_iter()
        .filter(|(name, _)| !std_prelude.contains(name) || local_value_names.contains(name))
        .collect();
    let finalize_ns = elapsed_ns(started);

    let final_registry = ctx.into_registry();
    Ok(CheckImplReport {
        typed_decls,
        scheme_env: user_scheme_env,
        registry: final_registry,
        timings: TypecheckPhaseTimings {
            registry_ns,
            inference_ns,
            solve_ns,
            scheme_env_ns,
            construction_ns,
            finalize_ns,
            solve_calls: solve_stats.solve_calls,
            constraints_processed: solve_stats.constraints_processed,
        },
    })
}

fn accumulate_typecheck_timings(target: &mut TypecheckPhaseTimings, source: TypecheckPhaseTimings) {
    target.registry_ns += source.registry_ns;
    target.inference_ns += source.inference_ns;
    target.solve_ns += source.solve_ns;
    target.scheme_env_ns += source.scheme_env_ns;
    target.construction_ns += source.construction_ns;
    target.finalize_ns += source.finalize_ns;
    target.solve_calls += source.solve_calls;
    target.constraints_processed += source.constraints_processed;
}

fn elapsed_ns(started: Instant) -> u64 {
    started.elapsed().as_nanos().min(u64::MAX as u128) as u64
}

fn top_level_value_names(program: &Program) -> HashSet<String> {
    program
        .decls
        .iter()
        .filter_map(|decl| match decl {
            Decl::Fun(d) => Some(d.name.clone()),
            Decl::Let(d) => Some(d.name.clone()),
            Decl::Mut(d) => Some(d.name.clone()),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Assert that StdPrelude::schemes() and evaluator::builtins::free_function_names()
    /// contain exactly the same keys. A divergence means a builtin either passes
    /// typechecking but fails at runtime ("undefined name") or vice versa.
    /// See METEL-5 / ADR-0027.
    #[test]
    fn stdprelude_and_evaluator_builtins_parity() {
        let prelude = StdPrelude::default();
        let prelude_names: std::collections::HashSet<&str> =
            prelude.schemes().keys().map(|s| s.as_str()).collect();
        let evaluator_names = crate::evaluator::builtins::free_function_names();

        let only_in_prelude: Vec<&&str> = prelude_names.difference(&evaluator_names).collect();
        let only_in_evaluator: Vec<&&str> = evaluator_names.difference(&prelude_names).collect();

        assert!(
            only_in_prelude.is_empty() && only_in_evaluator.is_empty(),
            "StdPrelude and evaluator builtin free-function sets diverged.\n\
             Only in StdPrelude (typechecks but fails at runtime): {:?}\n\
             Only in evaluator (available at runtime but rejected by typechecker): {:?}",
            only_in_prelude,
            only_in_evaluator,
        );
    }
}
