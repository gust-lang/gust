use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunnerKind {
    Parse,
    Typecheck,
    Evaluate,
    LoadProgram,
    LoadGraph,
    FullPipeline,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CorePreludeMode {
    Empty,
    Default,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExpectStatus {
    Success,
    ParseError,
    TypecheckError,
    RuntimeError,
    LoadError,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Expectation {
    pub status: ExpectStatus,
    pub code: Option<String>,
    pub contains: Option<String>,
    pub line: Option<usize>,
    pub col: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FixtureConfig {
    pub runner: RunnerKind,
    pub prelude: CorePreludeMode,
    pub expect: Expectation,
    pub program: ProgramChecks,
    pub graph: GraphChecks,
}

#[derive(Default)]
struct PartialConfig {
    runner: Option<RunnerKind>,
    prelude: Option<CorePreludeMode>,
    status: Option<ExpectStatus>,
    code: Option<String>,
    contains: Option<String>,
    line: Option<usize>,
    col: Option<usize>,
    program_imports: Option<usize>,
    program_decls: Option<usize>,
    graph_module_count: Option<usize>,
    graph_has_module_paths: Option<Vec<Vec<String>>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProgramChecks {
    pub imports: Option<usize>,
    pub decls: Option<usize>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GraphChecks {
    pub module_count: Option<usize>,
    pub has_module_paths: Vec<Vec<String>>,
}

pub fn resolve_fixture_config(suite: &str, fixture_path: &Path) -> FixtureConfig {
    let defaults = suite_defaults(suite);
    let mut partial = PartialConfig::default();

    if let Some(sidecar) = sidecar_path(fixture_path) {
        partial = parse_sidecar(&sidecar);
    } else if let Some(legacy) = parse_legacy_expectation(suite, fixture_path) {
        partial = legacy;
    }

    merge_config(defaults, partial)
}

pub fn main_source_path(fixture_path: &Path) -> PathBuf {
    if fixture_path.is_dir() {
        fixture_path.join("main.mtl")
    } else {
        fixture_path.to_path_buf()
    }
}

fn suite_defaults(suite: &str) -> FixtureConfig {
    match suite {
        "parsing" => FixtureConfig {
            runner: RunnerKind::Parse,
            prelude: CorePreludeMode::Empty,
            expect: Expectation::success(),
            program: ProgramChecks::default(),
            graph: GraphChecks::default(),
        },
        "typechecking" => FixtureConfig {
            runner: RunnerKind::Typecheck,
            prelude: CorePreludeMode::Default,
            expect: Expectation::success(),
            program: ProgramChecks::default(),
            graph: GraphChecks::default(),
        },
        "evaluator" => FixtureConfig {
            runner: RunnerKind::Evaluate,
            prelude: CorePreludeMode::Default,
            expect: Expectation::success(),
            program: ProgramChecks::default(),
            graph: GraphChecks::default(),
        },
        "module_loading" => FixtureConfig {
            runner: RunnerKind::FullPipeline,
            prelude: CorePreludeMode::Empty,
            expect: Expectation::success(),
            program: ProgramChecks::default(),
            graph: GraphChecks::default(),
        },
        "module_semantics" => FixtureConfig {
            runner: RunnerKind::FullPipeline,
            prelude: CorePreludeMode::Empty,
            expect: Expectation::success(),
            program: ProgramChecks::default(),
            graph: GraphChecks::default(),
        },
        other => panic!("unknown integration suite `{other}`"),
    }
}

impl Expectation {
    fn success() -> Self {
        Self {
            status: ExpectStatus::Success,
            code: None,
            contains: None,
            line: None,
            col: None,
        }
    }
}

fn merge_config(defaults: FixtureConfig, partial: PartialConfig) -> FixtureConfig {
    FixtureConfig {
        runner: partial.runner.unwrap_or(defaults.runner),
        prelude: partial.prelude.unwrap_or(defaults.prelude),
        expect: Expectation {
            status: partial.status.unwrap_or(defaults.expect.status),
            code: partial.code.or(defaults.expect.code),
            contains: partial.contains.or(defaults.expect.contains),
            line: partial.line.or(defaults.expect.line),
            col: partial.col.or(defaults.expect.col),
        },
        program: ProgramChecks {
            imports: partial.program_imports.or(defaults.program.imports),
            decls: partial.program_decls.or(defaults.program.decls),
        },
        graph: GraphChecks {
            module_count: partial.graph_module_count.or(defaults.graph.module_count),
            has_module_paths: partial
                .graph_has_module_paths
                .unwrap_or(defaults.graph.has_module_paths),
        },
    }
}

fn sidecar_path(fixture_path: &Path) -> Option<PathBuf> {
    if fixture_path.is_dir() {
        let sidecar = fixture_path.join("test.toml");
        sidecar.is_file().then_some(sidecar)
    } else {
        let sidecar = fixture_path.with_extension("toml");
        sidecar.is_file().then_some(sidecar)
    }
}

fn parse_sidecar(path: &Path) -> PartialConfig {
    let contents = fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("failed to read sidecar {}: {e}", path.display()));
    let mut partial = PartialConfig::default();
    let mut section = String::new();

    for raw_line in contents.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1].trim().to_string();
            continue;
        }

        let (key, value) = line
            .split_once('=')
            .unwrap_or_else(|| panic!("invalid sidecar line in {}: `{line}`", path.display()));
        let key = key.trim();
        let value = parse_scalar(value.trim());

        match section.as_str() {
            "" => match key {
                "runner" => partial.runner = Some(parse_runner(&value)),
                "prelude" => partial.prelude = Some(parse_prelude(&value)),
                other => panic!(
                    "unknown top-level sidecar key `{other}` in {}",
                    path.display()
                ),
            },
            "expect" => match key {
                "status" => partial.status = Some(parse_status(&value)),
                "code" => partial.code = Some(value),
                "contains" => partial.contains = Some(value),
                "line" => {
                    partial.line = Some(value.parse().unwrap_or_else(|e| {
                        panic!("invalid integer for `line` in {}: {e}", path.display())
                    }))
                }
                "col" => {
                    partial.col = Some(value.parse().unwrap_or_else(|e| {
                        panic!("invalid integer for `col` in {}: {e}", path.display())
                    }))
                }
                other => panic!("unknown expect sidecar key `{other}` in {}", path.display()),
            },
            "program" => match key {
                "imports" => {
                    partial.program_imports = Some(value.parse().unwrap_or_else(|e| {
                        panic!("invalid integer for `imports` in {}: {e}", path.display())
                    }))
                }
                "decls" => {
                    partial.program_decls = Some(value.parse().unwrap_or_else(|e| {
                        panic!("invalid integer for `decls` in {}: {e}", path.display())
                    }))
                }
                other => panic!(
                    "unknown program sidecar key `{other}` in {}",
                    path.display()
                ),
            },
            "graph" => match key {
                "module_count" => {
                    partial.graph_module_count = Some(value.parse().unwrap_or_else(|e| {
                        panic!(
                            "invalid integer for `module_count` in {}: {e}",
                            path.display()
                        )
                    }))
                }
                "has_module_paths" => {
                    partial.graph_has_module_paths = Some(
                        parse_list(&value)
                            .into_iter()
                            .map(|path| path.split("::").map(|seg| seg.to_string()).collect())
                            .collect(),
                    )
                }
                other => panic!("unknown graph sidecar key `{other}` in {}", path.display()),
            },
            other => panic!("unknown sidecar section `[{other}]` in {}", path.display()),
        }
    }

    partial
}

fn parse_scalar(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2 {
        trimmed[1..trimmed.len() - 1].to_string()
    } else {
        trimmed.to_string()
    }
}

fn parse_runner(value: &str) -> RunnerKind {
    match value {
        "parse" => RunnerKind::Parse,
        "typecheck" => RunnerKind::Typecheck,
        "evaluate" => RunnerKind::Evaluate,
        "load_program" => RunnerKind::LoadProgram,
        "load_graph" => RunnerKind::LoadGraph,
        "full_pipeline" => RunnerKind::FullPipeline,
        other => panic!("unknown runner `{other}`"),
    }
}

fn parse_prelude(value: &str) -> CorePreludeMode {
    match value {
        "empty" => CorePreludeMode::Empty,
        "default" => CorePreludeMode::Default,
        other => panic!("unknown prelude mode `{other}`"),
    }
}

fn parse_status(value: &str) -> ExpectStatus {
    match value {
        "success" => ExpectStatus::Success,
        "parse_error" => ExpectStatus::ParseError,
        "typecheck_error" => ExpectStatus::TypecheckError,
        "runtime_error" => ExpectStatus::RuntimeError,
        "load_error" => ExpectStatus::LoadError,
        other => panic!("unknown expectation status `{other}`"),
    }
}

fn parse_legacy_expectation(suite: &str, fixture_path: &Path) -> Option<PartialConfig> {
    if suite == "parsing" {
        return fixture_path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .filter(|stem| stem.starts_with("neg_"))
            .map(|_| PartialConfig {
                status: Some(ExpectStatus::ParseError),
                ..PartialConfig::default()
            });
    }

    let source_path = main_source_path(fixture_path);
    let source = fs::read_to_string(&source_path).ok()?;

    if suite == "typechecking" {
        for (idx, line) in source.lines().enumerate() {
            if let Some(code) = extract_annotation(line, "// ERROR[") {
                return Some(PartialConfig {
                    status: Some(ExpectStatus::TypecheckError),
                    code: Some(code),
                    line: Some(idx + 1),
                    ..PartialConfig::default()
                });
            }
        }
    }

    if suite == "evaluator" {
        for line in source.lines() {
            if let Some(expected) = extract_annotation(line, "// PARSE_ERROR[") {
                return Some(PartialConfig {
                    status: Some(ExpectStatus::ParseError),
                    contains: Some(expected),
                    ..PartialConfig::default()
                });
            }
            if let Some(expected) = extract_annotation(line, "// TYPECHECK_ERROR[") {
                return Some(PartialConfig {
                    status: Some(ExpectStatus::TypecheckError),
                    contains: Some(expected),
                    ..PartialConfig::default()
                });
            }
            if let Some(expected) = extract_annotation(line, "// RUNTIME_ERROR[") {
                return Some(PartialConfig {
                    status: Some(ExpectStatus::RuntimeError),
                    contains: Some(expected),
                    ..PartialConfig::default()
                });
            }
        }
    }

    None
}

fn extract_annotation(line: &str, marker: &str) -> Option<String> {
    let start = line.find(marker)?;
    let rest = &line[start + marker.len()..];
    let end = rest.find(']')?;
    Some(rest[..end].to_string())
}

fn parse_list(raw: &str) -> Vec<String> {
    let trimmed = raw.trim();
    if !(trimmed.starts_with('[') && trimmed.ends_with(']')) {
        panic!("expected list value, got `{trimmed}`");
    }
    let inner = &trimmed[1..trimmed.len() - 1];
    if inner.trim().is_empty() {
        return Vec::new();
    }
    inner
        .split(',')
        .map(|item| parse_scalar(item.trim()))
        .collect()
}
