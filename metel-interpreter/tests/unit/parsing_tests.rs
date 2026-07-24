use metel::ast::{Decl, ImportTree, PathRoot, Polarity};
use metel::parser;

#[test]
fn standalone_var_binding_parses() {
    let source = r#"
fun main() {
    var counter = 0;
}
"#;
    parser::parse(source, "standalone_var_binding.mtl").unwrap_or_else(|e| panic!("{e}"));
}

#[test]
fn mutable_for_in_binding_parses() {
    let source = r#"
fun main() {
    let values = [1, 2, 3];
    var total = 0;
    for (var item in values) {
        item += 1;
        total += item;
    }
}
"#;
    parser::parse(source, "mutable_for_in.mtl").unwrap_or_else(|e| panic!("{e}"));
}

#[test]
fn pointer_and_receiver_syntax_parses() {
    let source = r#"
struct Counter {
    value: i64,
}

extend Counter {
    fun increment(&var self) {
        self.value += 1;
    }

    fun current(&self) -> i64 {
        self.value
    }
}

fun main() {
    var value = 0;
    let ptr: &var i64 = &var value;
    ptr += 1;
    let read_only: &i64 = ptr;
    let _snapshot: i64 = read_only;
}
"#;
    parser::parse(source, "pointer_and_receiver_syntax.mtl").unwrap_or_else(|e| panic!("{e}"));
}

#[test]
fn bodyless_aspect_and_multi_extend_desugar() {
    let source = r#"
public aspect Copy2;
aspect Send;

extend Packet: Copy2, !Send;
"#;
    let program =
        parser::parse(source, "integrated_extend_surface.mtl").unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(program.decls.len(), 4);

    let Decl::Aspect(copy2) = &program.decls[0] else {
        panic!("expected first decl to be aspect");
    };
    assert_eq!(copy2.name, "Copy2");
    assert!(copy2.methods.is_empty());
    assert!(copy2.assoc_types.is_empty());

    let Decl::Impl(copy_impl) = &program.decls[2] else {
        panic!("expected third decl to be impl");
    };
    assert_eq!(copy_impl.aspect_name.as_deref(), Some("Copy2"));
    assert_eq!(copy_impl.polarity, Polarity::Positive);

    let Decl::Impl(send_impl) = &program.decls[3] else {
        panic!("expected fourth decl to be impl");
    };
    assert_eq!(send_impl.aspect_name.as_deref(), Some("Send"));
    assert_eq!(send_impl.polarity, Polarity::Negative);
}

fn assert_rejected(source: &str, filename: &str) {
    let err = parser::parse(source, filename).expect_err("expected parse error");
    let msg = format!("{err}");
    assert!(msg.contains("P0001"), "expected parse error, got: {msg}");
}

#[test]
fn rejects_legacy_impl_block_surface() {
    let source = r#"
impl Counter {
    fun bump(&var self) {}
}
"#;
    assert_rejected(source, "legacy_impl_block_surface.mtl");
}

#[test]
fn rejects_legacy_aspect_impl_surface() {
    let source = r#"
aspect Display {
    fun show(&self) -> String;
}

impl Display for Counter {
    fun show(&self) -> String { "x" }
}
"#;
    assert_rejected(source, "legacy_aspect_impl_surface.mtl");
}

#[test]
fn rejects_legacy_pub_and_mut_surface() {
    let source = r#"
pub struct Counter {
    pub value: i64,
}

fun main() {
    let mut counter = Counter { value = 0 };
    let ptr: &mut Counter = &mut counter;
    ptr.value += 1;
}
"#;
    assert_rejected(source, "legacy_pub_and_mut_surface.mtl");
}

#[test]
fn module_ast_preserves_roots_aliases_groups_and_globs() {
    let source = r#"
import std::math;
import root::parser::Ast;
import root::v1::Parser as ParserV1;
import root::v2::{Parser as ParserV2, Token};
import root::prelude::*;

export ast::Ast;

fun main() { }
"#;
    let program = parser::parse(source, "module_ast.mtl").unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(program.imports.len(), 5);
    assert_eq!(program.exports.len(), 1);

    assert_eq!(program.imports[0].path.root, PathRoot::Std);
    assert_eq!(
        program.imports[0].path.tree,
        ImportTree::Name {
            name: "math".to_string(),
            alias: None
        }
    );

    assert_eq!(program.imports[1].path.root, PathRoot::Root);
    assert_eq!(
        program.imports[1].path.tree,
        ImportTree::Path {
            name: "parser".to_string(),
            tree: Box::new(ImportTree::Name {
                name: "Ast".to_string(),
                alias: None
            }),
        }
    );

    assert_eq!(
        program.imports[2].path.tree,
        ImportTree::Path {
            name: "v1".to_string(),
            tree: Box::new(ImportTree::Name {
                name: "Parser".to_string(),
                alias: Some("ParserV1".to_string()),
            }),
        }
    );

    assert_eq!(
        program.imports[3].path.tree,
        ImportTree::Path {
            name: "v2".to_string(),
            tree: Box::new(ImportTree::Group(vec![
                ImportTree::Name {
                    name: "Parser".to_string(),
                    alias: Some("ParserV2".to_string()),
                },
                ImportTree::Name {
                    name: "Token".to_string(),
                    alias: None
                },
            ])),
        }
    );

    assert_eq!(
        program.imports[4].path.tree,
        ImportTree::Path {
            name: "prelude".to_string(),
            tree: Box::new(ImportTree::Glob)
        }
    );

    assert_eq!(
        program.exports[0].path.root,
        PathRoot::Name("ast".to_string())
    );
    assert_eq!(
        program.exports[0].path.tree,
        ImportTree::Name {
            name: "Ast".to_string(),
            alias: None
        }
    );
}

fn parse_error_message(filename: &str) -> String {
    let path = format!("tests/integration/sources/parsing/{filename}");
    let source =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("could not read {}: {}", path, e));
    match parser::parse(&source, filename) {
        Err(e) => format!("{e}"),
        Ok(_) => panic!("expected a parse error from {filename} but parsing succeeded"),
    }
}

#[test]
fn error_format_p0001_contains_filename() {
    let msg = parse_error_message("neg_01_syntax_error.mtl");
    assert!(
        msg.contains("neg_01_syntax_error.mtl"),
        "message was: {msg}"
    );
}

#[test]
fn error_format_p0001_contains_line_col() {
    let msg = parse_error_message("neg_01_syntax_error.mtl");
    assert!(
        msg.contains("neg_01_syntax_error.mtl:3:1"),
        "expected 'file:3:1' in message, got: {msg}"
    );
}

#[test]
fn error_format_p0001_contains_error_code() {
    let msg = parse_error_message("neg_01_syntax_error.mtl");
    assert!(
        msg.contains("P0001"),
        "expected '[P0001]' in message, got: {msg}"
    );
}

#[test]
fn error_format_p0001_does_not_contain_raw_byte_offset() {
    let msg = parse_error_message("neg_01_syntax_error.mtl");
    assert!(
        !msg.contains(".."),
        "message should not contain '..' (raw byte range), got: {msg}"
    );
}

#[test]
fn error_format_p0002_file_line_col() {
    let msg = parse_error_message("neg_02_int_overflow.mtl");
    assert!(
        msg.contains("P0002"),
        "expected '[P0002]' in message, got: {msg}"
    );
    assert!(
        msg.contains("neg_02_int_overflow.mtl:1:14"),
        "expected 'file:1:14' in message, got: {msg}"
    );
}

#[test]
fn error_format_mid_line_column() {
    let msg = parse_error_message("neg_03_float_invalid.mtl");
    assert!(
        msg.contains("neg_03_float_invalid.mtl:1:17"),
        "expected 'file:1:17' in message, got: {msg}"
    );
}

#[test]
fn error_format_line_counting_past_nine() {
    let msg = parse_error_message("neg_04_error_at_line_10.mtl");
    assert!(
        msg.contains("neg_04_error_at_line_10.mtl:10:1"),
        "expected 'file:10:1' in message, got: {msg}"
    );
}

#[test]
fn rejects_use_before_mod() {
    let msg = parse_error_message("neg_05_use_before_mod.mtl");
    assert!(msg.contains("P0001"), "expected parse error, got: {msg}");
}

#[test]
fn rejects_mod_after_declaration() {
    let msg = parse_error_message("neg_06_mod_after_decl.mtl");
    assert!(msg.contains("P0001"), "expected parse error, got: {msg}");
}

#[test]
fn rejects_old_fun_closure_syntax_in_expression_position() {
    let source = r#"
fun main() {
    let f = fun(x: i64) -> i64 { return x + 1; };
}
"#;

    let err = parser::parse(source, "old_fun_closure.mtl")
        .expect_err("expected parse error for old closure syntax");
    let msg = format!("{err}");
    assert!(msg.contains("P0001"), "expected parse error, got: {msg}");
}

#[test]
fn rejects_closure_without_arrow() {
    let source = r#"
fun main() {
    let f = (x: i64) { return x + 1; };
}
"#;

    let err = parser::parse(source, "no_arrow_closure.mtl")
        .expect_err("expected parse error for arrowless closure");
    let msg = format!("{err}");
    assert!(msg.contains("P0001"), "expected parse error, got: {msg}");
}

#[test]
fn parses_zero_arg_function_type_and_zero_arg_closure_together() {
    let source = r#"
fun takes_zero(f: () -> i64) -> i64 {
    return f();
}

fun main() -> i64 {
    return takes_zero(() -> i64 { return 42; });
}
"#;

    parser::parse(source, "zero_arg_closure_and_type.mtl").unwrap_or_else(|e| panic!("{e}"));
}

// METEL-191: the postfix `[]` array suffix must bind to a parenthesized/tuple
// type, not only to a named type. `(String, String)[]` previously failed with
// P0001 because the parser consumed the tuple and left the `[]` dangling.
#[test]
fn array_suffix_binds_to_tuple_type() {
    use metel::ast::{Decl, TypeExpr};

    let source = r#"
struct Env {
    vars: (String, String)[],
}

fun vars() -> (String, String)[] {
    return [];
}

fun takes(pairs: (String, String)[]) -> i64 {
    return 0;
}

fun annotates() {
    let xs: (i64, bool)[] = [];
}
"#;

    let program = parser::parse(source, "tuple_array_suffix.mtl")
        .unwrap_or_else(|e| panic!("(T, U)[] should parse: {e}"));

    let expect_tuple_array = |ty: &TypeExpr, ctx: &str| match ty {
        TypeExpr::Array(inner) => assert!(
            matches!(inner.as_ref(), TypeExpr::Tuple(elems) if elems.len() == 2),
            "{ctx}: expected Array(Tuple(2)), got Array({:?})",
            inner
        ),
        other => panic!("{ctx}: expected Array(Tuple(..)), got {other:?}"),
    };

    let mut saw_field = false;
    let mut saw_return = false;
    let mut saw_param = false;
    for decl in &program.decls {
        match decl {
            Decl::Struct(s) if s.name == "Env" => {
                expect_tuple_array(&s.fields[0].type_ann, "field");
                saw_field = true;
            }
            Decl::Fun(f) => match f.name.as_str() {
                "vars" => {
                    expect_tuple_array(f.return_type.as_ref().expect("vars return type"), "return");
                    saw_return = true;
                }
                "takes" => {
                    let ann = f.params[0].type_ann.as_ref().expect("param annotation");
                    expect_tuple_array(ann, "param");
                    saw_param = true;
                }
                _ => {}
            },
            _ => {}
        }
    }
    assert!(
        saw_field && saw_return && saw_param,
        "expected to inspect Env, vars() and takes()"
    );
}

#[test]
fn tuple_array_suffix_works_inside_nested_generic_arg() {
    let source = r#"
fun groups() -> List<(String, String)[]> {
    return List::new();
}
"#;

    parser::parse(source, "tuple_array_nested_generic_arg.mtl").unwrap_or_else(|e| {
        panic!("List<(T, U)[]> should parse when the array suffix binds to the tuple: {e}")
    });
}

// METEL-191 acceptance: a tuple as a generic type argument must also parse.
#[test]
fn tuple_type_as_generic_argument_parses() {
    let source = r#"
fun pairs() -> List<(String, String)> {
    return List::new();
}
"#;

    parser::parse(source, "tuple_generic_arg.mtl")
        .unwrap_or_else(|e| panic!("List<(String, String)> should parse: {e}"));
}
