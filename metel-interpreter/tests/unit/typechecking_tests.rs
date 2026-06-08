use metel::error::MetelError;
use metel::parser;
use metel::typechecker;

#[test]
fn question_mark_reports_the_postfix_column() {
    let source = r#"
struct ParseError { msg: String }
struct AppError { msg: String }

fun parse() -> Result<i64, ParseError> {
    Result::Err { error: ParseError { msg: "bad" } }
}

fun load() -> Result<i64, AppError> {
    let value = parse()?;
    Result::Ok { value: value }
}
"#;
    let program =
        parser::parse(source, "qmark_span.mtl").unwrap_or_else(|e| panic!("parse error: {e}"));
    match typechecker::check(program) {
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

impl Counter {
    fun increment(&mut self) {
        self.value += 1;
    }
}

fun main() {
    let counter = Counter { value: 0 };
    counter.increment();
}
"#;
    let program = parser::parse(source, "ref_mut_receiver_requires_mutable_binding.mtl")
        .unwrap_or_else(|e| panic!("parse error: {e}"));
    match typechecker::check(program) {
        Err(MetelError::TypeError { code, .. }) => {
            assert_eq!(format!("{code}"), "T0006");
        }
        Err(other) => panic!("expected TypeError, got: {other}"),
        Ok(_) => panic!("expected immutable receiver call to fail"),
    }
}
