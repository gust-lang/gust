use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::ast::{ImportTree, PathRoot, Program};
use crate::error::{MetelError, ParseErrorCode};
use crate::module_paths::resolve_path_root;
use crate::parser;

/// Supplies module source text to the loader (RFC-0058).
///
/// Abstracts the read step so the loader can serve source from the filesystem
/// (default), from compiled-in stdlib data, or from an in-memory overlay (LSP
/// unsaved buffers). Implementations receive both the logical module path (for
/// keyed lookups, e.g. embedded stdlib) and the resolved filesystem path (for
/// disk reads); a given implementation uses whichever it needs.
pub trait SourceProvider {
    fn read(&self, module_path: &[String], file_path: &Path) -> Result<String, MetelError>;
}

/// The default provider: reads module source from the filesystem. Behaviour is
/// identical to the loader's previous direct `fs::read_to_string` calls.
#[derive(Debug, Default, Clone, Copy)]
pub struct FsSourceProvider;

impl SourceProvider for FsSourceProvider {
    fn read(&self, _module_path: &[String], file_path: &Path) -> Result<String, MetelError> {
        fs::read_to_string(file_path).map_err(|e| {
            module_error(
                format!("failed to read module '{}': {e}", file_path.display()),
                file_path,
            )
        })
    }
}

/// Serves `std::…` modules from the binary-embedded stdlib sources, falling
/// through to the filesystem for everything else (RFC-0058 / METEL-181).
///
/// NOT YET WIRED as the default provider — that switch is part of removing the
/// virtual `std::core` (see the METEL-181 handoff note). Used today only by the
/// embedded-stdlib unit tests.
#[derive(Debug, Default, Clone, Copy)]
pub struct EmbeddedStdlibProvider {
    inner: FsSourceProvider,
}

impl SourceProvider for EmbeddedStdlibProvider {
    fn read(&self, module_path: &[String], file_path: &Path) -> Result<String, MetelError> {
        if let Some(src) = crate::stdlib::lookup(module_path) {
            return Ok(src.to_string());
        }
        self.inner.read(module_path, file_path)
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct LoadedModule {
    pub module_path: Vec<String>,
    pub file_path: PathBuf,
    pub program: Program,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ModuleGraph {
    pub root: PathBuf,
    pub modules: Vec<LoadedModule>,
    /// Maps alias module paths to their canonical module path.
    /// Populated when the same physical file is reachable via multiple logical paths
    /// (diamond dependency). e.g. `["right", "base"] -> ["left", "base"]`.
    pub path_aliases: HashMap<Vec<String>, Vec<String>>,
}

/// Load a module graph from `path` using the default filesystem provider.
pub fn load_root(path: impl AsRef<Path>) -> Result<ModuleGraph, MetelError> {
    load_root_with(path, &FsSourceProvider)
}

/// Load a module graph from `path`, reading source through `provider` (RFC-0058).
pub fn load_root_with<P: SourceProvider>(
    path: impl AsRef<Path>,
    provider: &P,
) -> Result<ModuleGraph, MetelError> {
    let root = canonicalize_existing(path.as_ref())?;
    let root_dir = root
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    let mut loader = Loader::new(root_dir, provider);
    // Synthesize the binary-embedded std:: modules into the graph ahead of user
    // code, so std::core is a real module flowing through the normal pipeline
    // (METEL-181) rather than a virtual injection.
    loader.load_embedded_stdlib()?;
    loader.load_module(root.clone(), Vec::new())?;
    Ok(ModuleGraph {
        root,
        modules: loader.modules,
        path_aliases: loader.path_aliases,
    })
}

/// Parse a single `.mtl` file and return its `Program`.
/// Single-file shim for tests that only need one-module typechecking.
#[allow(dead_code)] // public API used by module-loading test harness
pub fn load_program(path: impl AsRef<Path>) -> Result<Program, MetelError> {
    let path = canonicalize_existing(path.as_ref())?;
    let source = fs::read_to_string(&path)
        .map_err(|e| MetelError::internal(format!("could not read {}: {e}", path.display())))?;
    let filename = path.file_name().unwrap_or_default().to_string_lossy();
    crate::parser::parse(&source, &filename)
}

struct Loader<'a> {
    modules: Vec<LoadedModule>,
    visited: HashSet<PathBuf>,
    /// Maps each file's canonical path to the module path assigned on first visit.
    file_to_path: HashMap<PathBuf, Vec<String>>,
    /// Alias map: alternative module path → canonical module path.
    path_aliases: HashMap<Vec<String>, Vec<String>>,
    stack: Vec<PathBuf>,
    root_dir: PathBuf,
    provider: &'a dyn SourceProvider,
}

impl<'a> Loader<'a> {
    fn new(root_dir: PathBuf, provider: &'a dyn SourceProvider) -> Self {
        Self {
            modules: Vec::new(),
            visited: HashSet::new(),
            file_to_path: HashMap::new(),
            path_aliases: HashMap::new(),
            stack: Vec::new(),
            root_dir,
            provider,
        }
    }
}

impl Loader<'_> {
    /// Parse the binary-embedded std:: sources and add them to the graph as real
    /// modules (METEL-181). Their `module_path` is the logical path (e.g.
    /// `["std","core"]`); the synthetic `file_path` is for diagnostics only.
    fn load_embedded_stdlib(&mut self) -> Result<(), MetelError> {
        for module_path in crate::stdlib::module_paths() {
            let Some(source) = crate::stdlib::lookup(&module_path) else {
                continue;
            };
            let filename = format!("<embedded {}>", module_path.join("::"));
            let program = parser::parse(source, &filename)?;
            let file_path = PathBuf::from(&filename);
            self.visited.insert(file_path.clone());
            self.file_to_path
                .insert(file_path.clone(), module_path.clone());
            self.modules.push(LoadedModule {
                module_path,
                file_path,
                program,
            });
        }
        Ok(())
    }

    fn load_module(
        &mut self,
        file_path: PathBuf,
        module_path: Vec<String>,
    ) -> Result<(), MetelError> {
        let root_dir = self.root_dir.clone();
        if let Some(cycle_start) = self.stack.iter().position(|p| p == &file_path) {
            let mut chain: Vec<String> = self.stack[cycle_start..]
                .iter()
                .map(|p| p.display().to_string())
                .collect();
            chain.push(file_path.display().to_string());
            return Err(module_error(
                format!("circular module dependency: {}", chain.join(" -> ")),
                &file_path,
            ));
        }

        if self.visited.contains(&file_path) {
            // Same physical file reachable via a different logical path (diamond dependency).
            // Record the alias so the name resolver can dereference it. See ADR-0031.
            if let Some(canonical) = self.file_to_path.get(&file_path) {
                if *canonical != module_path {
                    self.path_aliases.insert(module_path, canonical.clone());
                }
            }
            return Ok(());
        }

        // `std` is reserved for the standard library; a user module may not occupy
        // that namespace. See RFC-0058 and the spec's "Reserved namespaces".
        validate_std_namespace(&module_path, &file_path)?;

        let source = self.provider.read(&module_path, &file_path)?;
        let filename = file_path.display().to_string();
        let program = parser::parse(&source, &filename)?;

        validate_super_root(&program, &module_path, &file_path)?;

        self.stack.push(file_path.clone());
        for import in &program.imports {
            if let Some((mod_segs, child_file)) =
                resolve_import_module(&file_path, &root_dir, &import.path.root, &import.path.tree)?
            {
                let child = canonicalize_existing(&child_file)?;
                let child_path = child_module_path(&module_path, &import.path.root, &mod_segs);
                self.load_module(child, child_path)?;
            }
        }
        self.stack.pop();

        self.visited.insert(file_path.clone());
        self.file_to_path
            .insert(file_path.clone(), module_path.clone());
        self.modules.push(LoadedModule {
            module_path,
            file_path,
            program,
        });
        Ok(())
    }
}

/// Compute the canonical module path for a child module.
///
/// For most root variants, delegates to `resolve_path_root` then appends `mod_segs`.
/// For `Name(n)`, however, `resolve_import_module` already puts `n` as `mod_segs[0]`,
/// so the base is just `parent` — using `resolve_path_root` here would double-include `n`.
/// See ADR-0023.
fn child_module_path(parent: &[String], root: &PathRoot, mod_segs: &[String]) -> Vec<String> {
    let base = match root {
        PathRoot::Name(_) => parent.to_vec(),
        _ => resolve_path_root(root, parent),
    };
    let mut path = base;
    path.extend_from_slice(mod_segs);
    path
}

fn canonicalize_existing(path: &Path) -> Result<PathBuf, MetelError> {
    path.canonicalize().map_err(|e| {
        module_error(
            format!("failed to resolve module '{}': {e}", path.display()),
            path,
        )
    })
}

/// Resolve an import declaration to a module file.
///
/// Returns `Ok(Some((segments, path)))` when a `.mtl` file is found.
/// Returns `Ok(None)` for `std::` imports (handled by `StdPrelude` in the typechecker)
/// and for glob/group imports that carry no resolvable file segment.
/// Returns `Err` if the import names a concrete module that cannot be found.
///
/// Path mapping: `::` separators map to `/` directory separators.
/// `import parser::ast::Ast` tries `parser/ast.mtl` first, then `parser.mtl` —
/// the longest matching prefix wins.
fn resolve_import_module(
    parent_file: &Path,
    root_dir: &Path,
    root: &PathRoot,
    tree: &ImportTree,
) -> Result<Option<(Vec<String>, PathBuf)>, MetelError> {
    let parent_dir = parent_file.parent().unwrap_or_else(|| Path::new("."));

    match root {
        PathRoot::Std => Ok(None),

        PathRoot::Root => {
            let segs = import_tree_segments(tree);
            resolve_in_dir(root_dir, &segs, parent_file)
        }

        PathRoot::Super => {
            let super_dir = if parent_dir == root_dir {
                root_dir.to_path_buf()
            } else {
                parent_dir.parent().unwrap_or(parent_dir).to_path_buf()
            };
            let segs = import_tree_segments(tree);
            resolve_in_dir(&super_dir, &segs, parent_file)
        }

        PathRoot::Self_ => {
            let segs = import_tree_segments(tree);
            resolve_in_dir(parent_dir, &segs, parent_file)
        }

        PathRoot::Name(name) => {
            let mut segs = vec![name.clone()];
            segs.extend(import_tree_segments(tree));
            resolve_in_dir(parent_dir, &segs, parent_file)
        }
    }
}

fn resolve_in_dir(
    dir: &Path,
    segs: &[String],
    source_file: &Path,
) -> Result<Option<(Vec<String>, PathBuf)>, MetelError> {
    if segs.is_empty() {
        return Ok(None);
    }
    match find_module_file(dir, segs) {
        Some(result) => Ok(Some(result)),
        None => Err(module_error(
            format!("cannot find module file for `{}`", segs.join("::")),
            source_file,
        )),
    }
}

/// Collect all identifier segments from an import tree in path order.
/// Stops at the terminal item(s) — returns their names as the last segment(s).
/// For `ast::Ast` → ["ast", "Ast"]; for `ast::{A, B}` → ["ast"]; for `*` → [].
fn import_tree_segments(tree: &ImportTree) -> Vec<String> {
    match tree {
        ImportTree::Name { name, .. } => vec![name.clone()],
        ImportTree::Path { name, tree } => {
            let mut segs = vec![name.clone()];
            segs.extend(import_tree_segments(tree));
            segs
        }
        ImportTree::Group(_) | ImportTree::Glob => vec![],
    }
}

/// Try path prefixes from longest to shortest, returning the first `.mtl` found.
fn find_module_file(base_dir: &Path, segs: &[String]) -> Option<(Vec<String>, PathBuf)> {
    for len in (1..=segs.len()).rev() {
        let prefix = &segs[..len];
        let mut candidate = base_dir.to_path_buf();
        for seg in prefix {
            candidate = candidate.join(seg);
        }
        let file = candidate.with_extension("mtl");
        if file.exists() {
            return Some((prefix.to_vec(), file));
        }
    }
    None
}

/// Reject any user module whose path begins with `std`. The `std` namespace is
/// reserved for the standard library; enforcing it here (the file-discovery
/// layer) matches the `std` keyword reservation in the grammar. See RFC-0058.
fn validate_std_namespace(module_path: &[String], file_path: &Path) -> Result<(), MetelError> {
    if module_path.first().map(String::as_str) == Some("std") {
        return Err(module_error(
            "module path `std::…` is reserved for the standard library",
            file_path,
        ));
    }
    Ok(())
}

fn validate_super_root(
    program: &Program,
    module_path: &[String],
    file_path: &Path,
) -> Result<(), MetelError> {
    if !module_path.is_empty() {
        return Ok(());
    }

    for import in &program.imports {
        if import.path.root == PathRoot::Super || import_tree_contains_super(&import.path.tree) {
            return Err(module_error(
                "`super::` is invalid from the root module",
                file_path,
            ));
        }
    }

    Ok(())
}

fn import_tree_contains_super(tree: &ImportTree) -> bool {
    match tree {
        ImportTree::Name { .. } | ImportTree::Glob => false,
        ImportTree::Group(trees) => trees.iter().any(import_tree_contains_super),
        ImportTree::Path { tree, .. } => import_tree_contains_super(tree),
    }
}

fn module_error(message: impl Into<String>, path: &Path) -> MetelError {
    MetelError::ParseError {
        code: ParseErrorCode::P0001,
        message: message.into(),
        start: 0,
        end: 0,
        filename: path.display().to_string(),
        line: 1,
        col: 1,
        source_line: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn std_namespace_is_reserved_for_user_modules() {
        let path = Path::new("std.mtl");
        let err = validate_std_namespace(&["std".to_string()], path)
            .expect_err("a user module named `std` must be rejected");
        assert!(
            err.to_string().contains("reserved for the standard library"),
            "got: {err}"
        );
        // Nested under std is also rejected.
        assert!(validate_std_namespace(&["std".to_string(), "io".to_string()], path).is_err());
    }

    #[test]
    fn non_std_module_paths_are_allowed() {
        let path = Path::new("foo.mtl");
        assert!(validate_std_namespace(&[], path).is_ok());
        assert!(validate_std_namespace(&["foo".to_string()], path).is_ok());
        // `standard` is a different name, not the reserved `std` segment.
        assert!(validate_std_namespace(&["standard".to_string()], path).is_ok());
    }

    #[test]
    fn source_provider_overlay_supplies_in_memory_source() {
        // Proves the SourceProvider abstraction supports an in-memory overlay
        // (the LSP unsaved-buffer use case) without touching disk.
        struct Overlay;
        impl SourceProvider for Overlay {
            fn read(&self, module_path: &[String], _file: &Path) -> Result<String, MetelError> {
                assert_eq!(module_path, &["greeter".to_string()]);
                Ok("pub fun hi() {}".to_string())
            }
        }
        let provider = Overlay;
        let src = provider
            .read(&["greeter".to_string()], Path::new("ignored.mtl"))
            .unwrap();
        assert!(src.contains("fun hi"));
    }
}
