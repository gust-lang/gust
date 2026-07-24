use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};

use metel::error::MetelError;
use metel::{module_loader, name_resolver, path_normalizer, typechecker};

/// Typecheck a single source string through the full module pipeline (load +
/// resolve + normalize + check_graph), the same path the shipped binary uses.
///
/// The legacy single-program path (`typechecker::check`) was removed once it
/// became the sole remaining surface-name consumer in the SymbolId migration
/// (METEL-185 / ADR-0041), so these unit tests drive the real pipeline by writing
/// the source to a temporary file and loading it as a module graph.
fn check_source(source: &str) -> Result<(), MetelError> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("metel_unit_{}_{n}.mtl", std::process::id()));
    {
        let mut file = std::fs::File::create(&path).expect("create temp fixture");
        file.write_all(source.as_bytes())
            .expect("write temp fixture");
    }
    let result = (|| {
        let graph = module_loader::load_root(&path)?;
        let names = name_resolver::resolve(&graph)?;
        let normalized = path_normalizer::normalize(graph, &names)?;
        typechecker::check_graph(&normalized, &names, &typechecker::CorePrelude::default())
            .map(|_| ())
    })();
    let _ = std::fs::remove_file(&path);
    result
}

#[test]
fn question_mark_reports_the_postfix_column() {
    let source = r#"
struct ParseError { msg: String }
struct AppError { msg: String }

fun parse() -> Result<i64, ParseError> {
    Result::Err { error= ParseError { msg= "bad" } }
}

fun load() -> Result<i64, AppError> {
    let value = parse()?;
    Result::Ok { value= value }
}
"#;
    match check_source(source) {
        Err(MetelError::TypeError {
            code, line, col, ..
        }) => {
            assert_eq!(format!("{code}"), "T0007");
            assert_eq!(line, 10);
            assert_eq!(col, 24);
        }
        Err(other) => panic!("expected TypeError, got: {other}"),
        Ok(_) => panic!("expected type error for missing From impl"),
    }
}

#[test]
fn ref_mut_receiver_requires_mutable_binding() {
    let source = r#"
struct Counter {
    value: i64,
}

extend Counter {
    fun increment(&var self) {
        self.value += 1;
    }
}

fun main() {
    let counter = Counter { value= 0 };
    counter.increment();
}
"#;
    match check_source(source) {
        Err(MetelError::TypeError { code, .. }) => {
            assert_eq!(format!("{code}"), "T0006");
        }
        Err(other) => panic!("expected TypeError, got: {other}"),
        Ok(_) => panic!("expected immutable receiver call to fail"),
    }
}
