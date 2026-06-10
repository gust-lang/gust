use crate::error::{MetelError, RuntimeErrorCode};

use super::display::{format_value, value_to_display_string};
use super::{
    RuntimeCallable, RuntimeMethod, RuntimeRegistry, RuntimeSignature, RuntimeTypePattern,
    RuntimeTypeRef, Value,
};

/// Built-in primitive type names that implement `Display` (expose `to_string`).
/// The runtime formats all of these via `value_to_display_string`. Mirrors the
/// typechecker-side list in `typechecker::registry`; the two are unified when
/// `std::core` becomes a real module (METEL-181).
const DISPLAYABLE_PRIMITIVE_NAMES: &[&str] = &[
    "i8", "i16", "i32", "i64", "u8", "u16", "u32", "u64", "f32", "f64", "boolean", "Char",
    "String",
];

fn numeric_as_i128(v: &Value) -> Option<i128> {
    match v {
        Value::I8(n) => Some(*n as i128),
        Value::I16(n) => Some(*n as i128),
        Value::I32(n) => Some(*n as i128),
        Value::I64(n) => Some(*n as i128),
        Value::U8(n) => Some(*n as i128),
        Value::U16(n) => Some(*n as i128),
        Value::U32(n) => Some(*n as i128),
        Value::U64(n) => Some(*n as i128),
        Value::F32(f) => Some(*f as i128),
        Value::F64(f) => Some(*f as i128),
        _ => None,
    }
}

fn numeric_as_f64_val(v: &Value) -> Option<f64> {
    match v {
        Value::I8(n) => Some(*n as f64),
        Value::I16(n) => Some(*n as f64),
        Value::I32(n) => Some(*n as f64),
        Value::I64(n) => Some(*n as f64),
        Value::U8(n) => Some(*n as f64),
        Value::U16(n) => Some(*n as f64),
        Value::U32(n) => Some(*n as f64),
        Value::U64(n) => Some(*n as f64),
        Value::F32(f) => Some(*f as f64),
        Value::F64(f) => Some(*f),
        _ => None,
    }
}

// ── Native host implementations (METEL-182) ────────────────────────────────
// Each stdlib `native(@…)` function dispatches to the matching host fn here,
// selected by its `NativeKey`. These mirror the legacy `register_core!`
// builtins and replace them once `std::core` is a real module (METEL-181).

use crate::native_keys::NativeKey;

fn native_print(args: Vec<Value>, span: &crate::ast::Span) -> Result<Value, MetelError> {
    let v = args
        .first()
        .ok_or_else(|| MetelError::internal("print: expected one argument"))?;
    let s = value_to_display_string(v).ok_or_else(|| {
        MetelError::panic(
            RuntimeErrorCode::R0009,
            "print: value does not implement Display",
            span,
        )
    })?;
    print!("{s}");
    Ok(Value::Unit)
}

fn native_println(args: Vec<Value>, span: &crate::ast::Span) -> Result<Value, MetelError> {
    let v = args
        .first()
        .ok_or_else(|| MetelError::internal("println: expected one argument"))?;
    let s = value_to_display_string(v).ok_or_else(|| {
        MetelError::panic(
            RuntimeErrorCode::R0009,
            "println: value does not implement Display",
            span,
        )
    })?;
    println!("{s}");
    Ok(Value::Unit)
}

fn native_dbg(args: Vec<Value>, _span: &crate::ast::Span) -> Result<Value, MetelError> {
    match args.first() {
        Some(val) => {
            eprintln!("[dbg] {}", format_value(val));
            Ok(val.clone())
        }
        None => Err(MetelError::internal("dbg: expected one argument")),
    }
}

fn native_assert(args: Vec<Value>, span: &crate::ast::Span) -> Result<Value, MetelError> {
    match args.first() {
        Some(Value::Boolean(true)) => Ok(Value::Unit),
        Some(Value::Boolean(false)) => Err(MetelError::panic(
            RuntimeErrorCode::R0013,
            "assertion failed",
            span,
        )),
        _ => Err(MetelError::internal("assert: expected boolean argument")),
    }
}

fn native_assert_msg(args: Vec<Value>, span: &crate::ast::Span) -> Result<Value, MetelError> {
    match (args.first(), args.get(1)) {
        (Some(Value::Boolean(true)), _) => Ok(Value::Unit),
        (Some(Value::Boolean(false)), Some(Value::Str(msg))) => {
            Err(MetelError::panic(RuntimeErrorCode::R0013, msg.clone(), span))
        }
        (Some(Value::Boolean(false)), _) => Err(MetelError::panic(
            RuntimeErrorCode::R0013,
            "assertion failed",
            span,
        )),
        _ => Err(MetelError::internal(
            "assert_msg: expected (boolean, String) arguments",
        )),
    }
}

fn native_clock(_args: Vec<Value>, _span: &crate::ast::Span) -> Result<Value, MetelError> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    Ok(Value::I64(ms))
}

fn native_string_len(args: Vec<Value>, _span: &crate::ast::Span) -> Result<Value, MetelError> {
    match args.first() {
        Some(Value::Str(s)) => Ok(Value::I64(s.chars().count() as i64)),
        _ => Err(MetelError::internal("string_len: expected String argument")),
    }
}

fn native_string_concat(args: Vec<Value>, _span: &crate::ast::Span) -> Result<Value, MetelError> {
    match (args.first(), args.get(1)) {
        (Some(Value::Str(a)), Some(Value::Str(b))) => Ok(Value::Str(a.clone() + b)),
        _ => Err(MetelError::internal(
            "string_concat: expected two String arguments",
        )),
    }
}

/// The host implementation for a stdlib `native` function, looked up by its
/// lowered [`NativeKey`]. Total over the closed enum — every variant maps to a
/// host fn (enforced by the coverage test).
pub(super) fn native_host_impl(key: NativeKey) -> RuntimeCallable {
    let (label, fun): (&str, fn(Vec<Value>, &crate::ast::Span) -> Result<Value, MetelError>) =
        match key {
            NativeKey::StdCorePrint => ("std::core::print", native_print),
            NativeKey::StdCorePrintln => ("std::core::println", native_println),
            NativeKey::StdCoreDbg => ("std::core::dbg", native_dbg),
            NativeKey::StdCoreAssert => ("std::core::assert", native_assert),
            NativeKey::StdCoreAssertMsg => ("std::core::assert_msg", native_assert_msg),
            NativeKey::StdCoreClock => ("std::core::clock", native_clock),
            NativeKey::StdCoreStringLen => ("std::core::string_len", native_string_len),
            NativeKey::StdCoreStringConcat => {
                ("std::core::string_concat", native_string_concat)
            }
        };
    RuntimeCallable::Intrinsic {
        label: label.to_string(),
        fun,
    }
}

/// Register the std::core free functions by parsing the embedded core.mtl and
/// binding each `native` declaration to its host implementation (METEL-181).
/// `stdlib/core.mtl` + the `NativeKey` enum are the single source of truth;
/// there is no hand-maintained list to keep in sync with the typechecker (the
/// prelude derives its schemes from the same source). This serves the
/// single-program pipeline; the module-graph pipeline additionally evaluates
/// std::core as a real module.
fn register_core_natives_from_embedded(runtime: &mut RuntimeRegistry) {
    let core_path = ["std".to_string(), "core".to_string()];
    let Some(source) = crate::stdlib::lookup(&core_path) else {
        return;
    };
    let program = crate::parser::parse(source, "<embedded std::core>")
        .expect("embedded std::core must parse; it is compiled into the binary");
    for decl in &program.decls {
        let crate::ast::Decl::Fun(fun) = decl else {
            continue;
        };
        let Some(binding) = &fun.native else { continue };
        let key = NativeKey::from_path(&binding.key_path).unwrap_or_else(|| {
            panic!(
                "embedded std::core declares unknown native binding @{}",
                binding.key_path.join(".")
            )
        });
        runtime.register_std_core_value(fun.name.clone(), Value::Callable(native_host_impl(key)));
    }
}

pub(super) fn register_builtins(runtime: &mut RuntimeRegistry) {
    fn named(name: &str) -> RuntimeTypeRef {
        RuntimeTypeRef::Named(name.to_string())
    }

    fn method(
        label: &str,
        receiver: Option<crate::ast::ReceiverKind>,
        params: &[&str],
        ret: Option<&str>,
        body: RuntimeCallable,
    ) -> RuntimeMethod {
        RuntimeMethod {
            label: label.to_string(),
            receiver,
            signature: RuntimeSignature {
                params: params.iter().map(|name| named(name)).collect(),
                ret: ret.map(named),
            },
            body,
        }
    }

    fn intrinsic(
        label: &str,
        fun: fn(Vec<Value>, &crate::ast::Span) -> Result<Value, MetelError>,
    ) -> RuntimeCallable {
        RuntimeCallable::Intrinsic {
            label: label.to_string(),
            fun,
        }
    }

    fn builtin_value(
        label: &str,
        fun: fn(Vec<Value>, &crate::ast::Span) -> Result<Value, MetelError>,
    ) -> RuntimeCallable {
        intrinsic(label, fun)
    }

    macro_rules! register_type_value {
        ($type_name:expr, $method_name:expr, $value:expr) => {
            runtime.register_type_value($type_name, $method_name, $value);
        };
    }
    macro_rules! register_inherent {
        ($type_name:expr, $method_name:expr, $value:expr) => {
            runtime.register_inherent_method($type_name, $method_name, $value);
        };
    }
    macro_rules! register_pattern {
        ($pattern:expr, $method_name:expr, $value:expr) => {
            runtime.register_pattern_method($pattern, $method_name, $value);
        };
    }
    macro_rules! register_aspect {
        ($type_name:expr, $aspect_name:expr, [$($type_arg:expr),* $(,)?], $method_name:expr, $value:expr) => {
            runtime.register_aspect_method(
                $type_name,
                $aspect_name,
                None,
                vec![$($type_arg.to_string()),*],
                $method_name,
                $value,
            );
        };
    }
    macro_rules! register_from {
        ($target:expr, $source:expr, $value:expr) => {
            register_aspect!($target, "From", [$source], "from", $value);
        };
    }

    // std::core free functions (print/println/dbg/assert/assert_msg/clock/
    // string_len/string_concat) — derived from the embedded core.mtl natives.
    register_core_natives_from_embedded(runtime);

    // to_string() methods for built-in Display types. Every Displayable
    // primitive (all numeric types plus boolean/Char/String) uses the same
    // runtime formatter, so register them with one shared body. The runtime
    // type name of the receiver (runtime_type_name) selects the right entry.
    fn display_to_string(
        args: Vec<Value>,
        span: &crate::ast::Span,
    ) -> Result<Value, MetelError> {
        match args.first() {
            Some(v) => value_to_display_string(v).map(Value::Str).ok_or_else(|| {
                MetelError::panic(
                    RuntimeErrorCode::R0009,
                    "to_string: value does not implement Display",
                    span,
                )
            }),
            None => Err(MetelError::internal("to_string: expected a receiver")),
        }
    }
    for type_name in DISPLAYABLE_PRIMITIVE_NAMES {
        register_aspect!(
            *type_name,
            "Display",
            [],
            "to_string",
            method(
                "Display::to_string",
                Some(crate::ast::ReceiverKind::Value),
                &[],
                Some("String"),
                builtin_value("Display::to_string", display_to_string),
            )
        );
    }

    register_pattern!(
        RuntimeTypePattern::Str,
        "len",
        method(
            "String::len",
            Some(crate::ast::ReceiverKind::Value),
            &[],
            Some("i64"),
            builtin_value("String::len", |args, _span| {
                match args.first() {
                    Some(Value::Str(s)) => Ok(Value::I64(s.chars().count() as i64)),
                    _ => Err(MetelError::internal("String::len: expected String")),
                }
            }),
        )
    );

    register_pattern!(
        RuntimeTypePattern::Array,
        "len",
        method(
            "Array::len",
            Some(crate::ast::ReceiverKind::Value),
            &[],
            Some("i64"),
            builtin_value("Array::len", |args, _span| {
                match args.first() {
                    Some(Value::Array(arr)) => Ok(Value::I64(arr.borrow().len() as i64)),
                    _ => Err(MetelError::internal("Array::len: expected array")),
                }
            }),
        )
    );

    // Numeric From impls: full cross-product of all numeric types.
    // Fallback generic builtins (for any remaining dispatch paths).
    register_inherent!(
        "i64",
        "from",
        method(
            "i64::from",
            None,
            &["numeric"],
            Some("i64"),
            builtin_value("i64::from", |args, _span| {
                match args.first().and_then(numeric_as_i128) {
                    Some(n) => Ok(Value::I64(n as i64)),
                    None => Err(MetelError::internal("i64::from: expected numeric")),
                }
            }),
        )
    );
    register_inherent!(
        "f64",
        "from",
        method(
            "f64::from",
            None,
            &["numeric"],
            Some("f64"),
            builtin_value("f64::from", |args, _span| {
                match args.first().and_then(numeric_as_f64_val) {
                    Some(f) => Ok(Value::F64(f)),
                    None => Err(MetelError::internal("f64::from: expected numeric")),
                }
            }),
        )
    );

    // Specific-key From impls (evaluated before generic fallbacks).
    macro_rules! from_int {
        ($target:literal, $source:literal, $out:expr) => {
            register_from!(
                $target,
                $source,
                method(
                    concat!($target, "::From<", $source, ">::from"),
                    None,
                    &[$source],
                    Some($target),
                    builtin_value(
                        concat!($target, "::From<", $source, ">::from"),
                        |args, _span| {
                            match args.first().and_then(|v| numeric_as_i128(v)) {
                                Some(n) => Ok($out(n)),
                                None => Err(MetelError::internal(concat!(
                                    $target,
                                    "::From<",
                                    $source,
                                    ">::from: unexpected argument"
                                ))),
                            }
                        }
                    ),
                )
            );
        };
    }
    macro_rules! from_float {
        ($target:literal, $source:literal, $out:expr) => {
            register_from!(
                $target,
                $source,
                method(
                    concat!($target, "::From<", $source, ">::from"),
                    None,
                    &[$source],
                    Some($target),
                    builtin_value(
                        concat!($target, "::From<", $source, ">::from"),
                        |args, _span| {
                            match args.first().and_then(|v| numeric_as_f64_val(v)) {
                                Some(f) => Ok($out(f)),
                                None => Err(MetelError::internal(concat!(
                                    $target,
                                    "::From<",
                                    $source,
                                    ">::from: unexpected argument"
                                ))),
                            }
                        }
                    ),
                )
            );
        };
    }

    // i8
    from_int!("i8", "i16", |n: i128| Value::I8(n as i8));
    from_int!("i8", "i32", |n: i128| Value::I8(n as i8));
    from_int!("i8", "i64", |n: i128| Value::I8(n as i8));
    from_int!("i8", "u8", |n: i128| Value::I8(n as i8));
    from_int!("i8", "u16", |n: i128| Value::I8(n as i8));
    from_int!("i8", "u32", |n: i128| Value::I8(n as i8));
    from_int!("i8", "u64", |n: i128| Value::I8(n as i8));
    from_int!("i8", "f32", |n: i128| Value::I8(n as i8));
    from_int!("i8", "f64", |n: i128| Value::I8(n as i8));
    // i16
    from_int!("i16", "i8", |n: i128| Value::I16(n as i16));
    from_int!("i16", "i32", |n: i128| Value::I16(n as i16));
    from_int!("i16", "i64", |n: i128| Value::I16(n as i16));
    from_int!("i16", "u8", |n: i128| Value::I16(n as i16));
    from_int!("i16", "u16", |n: i128| Value::I16(n as i16));
    from_int!("i16", "u32", |n: i128| Value::I16(n as i16));
    from_int!("i16", "u64", |n: i128| Value::I16(n as i16));
    from_int!("i16", "f32", |n: i128| Value::I16(n as i16));
    from_int!("i16", "f64", |n: i128| Value::I16(n as i16));
    // i32
    from_int!("i32", "i8", |n: i128| Value::I32(n as i32));
    from_int!("i32", "i16", |n: i128| Value::I32(n as i32));
    from_int!("i32", "i64", |n: i128| Value::I32(n as i32));
    from_int!("i32", "u8", |n: i128| Value::I32(n as i32));
    from_int!("i32", "u16", |n: i128| Value::I32(n as i32));
    from_int!("i32", "u32", |n: i128| Value::I32(n as i32));
    from_int!("i32", "u64", |n: i128| Value::I32(n as i32));
    from_int!("i32", "f32", |n: i128| Value::I32(n as i32));
    from_int!("i32", "f64", |n: i128| Value::I32(n as i32));
    // i64
    from_int!("i64", "i8", |n: i128| Value::I64(n as i64));
    from_int!("i64", "i16", |n: i128| Value::I64(n as i64));
    from_int!("i64", "i32", |n: i128| Value::I64(n as i64));
    from_int!("i64", "u8", |n: i128| Value::I64(n as i64));
    from_int!("i64", "u16", |n: i128| Value::I64(n as i64));
    from_int!("i64", "u32", |n: i128| Value::I64(n as i64));
    from_int!("i64", "u64", |n: i128| Value::I64(n as i64));
    from_int!("i64", "f32", |n: i128| Value::I64(n as i64));
    from_int!("i64", "f64", |n: i128| Value::I64(n as i64));
    // u8
    from_int!("u8", "i8", |n: i128| Value::U8(n as u8));
    from_int!("u8", "i16", |n: i128| Value::U8(n as u8));
    from_int!("u8", "i32", |n: i128| Value::U8(n as u8));
    from_int!("u8", "i64", |n: i128| Value::U8(n as u8));
    from_int!("u8", "u16", |n: i128| Value::U8(n as u8));
    from_int!("u8", "u32", |n: i128| Value::U8(n as u8));
    from_int!("u8", "u64", |n: i128| Value::U8(n as u8));
    from_int!("u8", "f32", |n: i128| Value::U8(n as u8));
    from_int!("u8", "f64", |n: i128| Value::U8(n as u8));
    // u16
    from_int!("u16", "i8", |n: i128| Value::U16(n as u16));
    from_int!("u16", "i16", |n: i128| Value::U16(n as u16));
    from_int!("u16", "i32", |n: i128| Value::U16(n as u16));
    from_int!("u16", "i64", |n: i128| Value::U16(n as u16));
    from_int!("u16", "u8", |n: i128| Value::U16(n as u16));
    from_int!("u16", "u32", |n: i128| Value::U16(n as u16));
    from_int!("u16", "u64", |n: i128| Value::U16(n as u16));
    from_int!("u16", "f32", |n: i128| Value::U16(n as u16));
    from_int!("u16", "f64", |n: i128| Value::U16(n as u16));
    // u32
    from_int!("u32", "i8", |n: i128| Value::U32(n as u32));
    from_int!("u32", "i16", |n: i128| Value::U32(n as u32));
    from_int!("u32", "i32", |n: i128| Value::U32(n as u32));
    from_int!("u32", "i64", |n: i128| Value::U32(n as u32));
    from_int!("u32", "u8", |n: i128| Value::U32(n as u32));
    from_int!("u32", "u16", |n: i128| Value::U32(n as u32));
    from_int!("u32", "u64", |n: i128| Value::U32(n as u32));
    from_int!("u32", "f32", |n: i128| Value::U32(n as u32));
    from_int!("u32", "f64", |n: i128| Value::U32(n as u32));
    // u64
    from_int!("u64", "i8", |n: i128| Value::U64(n as u64));
    from_int!("u64", "i16", |n: i128| Value::U64(n as u64));
    from_int!("u64", "i32", |n: i128| Value::U64(n as u64));
    from_int!("u64", "i64", |n: i128| Value::U64(n as u64));
    from_int!("u64", "u8", |n: i128| Value::U64(n as u64));
    from_int!("u64", "u16", |n: i128| Value::U64(n as u64));
    from_int!("u64", "u32", |n: i128| Value::U64(n as u64));
    from_int!("u64", "f32", |n: i128| Value::U64(n as u64));
    from_int!("u64", "f64", |n: i128| Value::U64(n as u64));
    // f32
    from_float!("f32", "i8", |f: f64| Value::F32(f as f32));
    from_float!("f32", "i16", |f: f64| Value::F32(f as f32));
    from_float!("f32", "i32", |f: f64| Value::F32(f as f32));
    from_float!("f32", "i64", |f: f64| Value::F32(f as f32));
    from_float!("f32", "u8", |f: f64| Value::F32(f as f32));
    from_float!("f32", "u16", |f: f64| Value::F32(f as f32));
    from_float!("f32", "u32", |f: f64| Value::F32(f as f32));
    from_float!("f32", "u64", |f: f64| Value::F32(f as f32));
    from_float!("f32", "f64", |f: f64| Value::F32(f as f32));
    // f64
    from_float!("f64", "i8", |f: f64| Value::F64(f));
    from_float!("f64", "i16", |f: f64| Value::F64(f));
    from_float!("f64", "i32", |f: f64| Value::F64(f));
    from_float!("f64", "i64", |f: f64| Value::F64(f));
    from_float!("f64", "u8", |f: f64| Value::F64(f));
    from_float!("f64", "u16", |f: f64| Value::F64(f));
    from_float!("f64", "u32", |f: f64| Value::F64(f));
    from_float!("f64", "u64", |f: f64| Value::F64(f));
    from_float!("f64", "f32", |f: f64| Value::F64(f));

    register_from!(
        "u32",
        "Char",
        method(
            "u32::From<Char>::from",
            None,
            &["Char"],
            Some("u32"),
            builtin_value("u32::From<Char>::from", |args, _span| {
                match args.first() {
                    Some(Value::Char(c)) => Ok(Value::U32(*c as u32)),
                    _ => Err(MetelError::internal("u32::From<Char>::from: expected Char")),
                }
            }),
        )
    );
    register_from!(
        "Char",
        "u32",
        method(
            "Char::From<u32>::from",
            None,
            &["u32"],
            Some("Char"),
            builtin_value("Char::From<u32>::from", |args, span| {
                match args.first() {
                    Some(Value::U32(n)) => char::from_u32(*n).map(Value::Char).ok_or_else(|| {
                        MetelError::panic(
                            RuntimeErrorCode::R0009,
                            format!("u32 value {n} is not a valid Unicode scalar"),
                            span,
                        )
                    }),
                    _ => Err(MetelError::internal("Char::From<u32>::from: expected u32")),
                }
            }),
        )
    );

    // ── List<T> constructors ──────────────────────────────────────────────────

    // Helper: build a List Value from a backing Rc array.
    // List<T> is represented as Value::Struct { name: "List", fields: { "inner": Value::Array(rc) } }

    register_type_value!(
        "List",
        "new",
        method(
            "List::new",
            None,
            &[],
            Some("List<T>"),
            builtin_value("List::new", |_args, _span| {
                use std::cell::RefCell;
                use std::rc::Rc;
                let mut fields = std::collections::HashMap::new();
                fields.insert(
                    "inner".to_string(),
                    Value::Array(Rc::new(RefCell::new(vec![]))),
                );
                Ok(Value::Struct {
                    name: "List".to_string(),
                    fields,
                })
            }),
        )
    );

    register_type_value!(
        "List",
        "from",
        method(
            "List::from",
            None,
            &["T[]"],
            Some("List<T>"),
            builtin_value("List::from", |args, _span| {
                use std::cell::RefCell;
                use std::rc::Rc;
                match args.first() {
                    Some(Value::Array(src)) => {
                        let copy = src.borrow().clone();
                        let mut fields = std::collections::HashMap::new();
                        fields.insert(
                            "inner".to_string(),
                            Value::Array(Rc::new(RefCell::new(copy))),
                        );
                        Ok(Value::Struct {
                            name: "List".to_string(),
                            fields,
                        })
                    }
                    _ => Err(MetelError::internal("List::from: expected array argument")),
                }
            }),
        )
    );

    // ── List<T> instance methods (keyed as "List::method") ───────────────────

    register_inherent!(
        "List",
        "push",
        method(
            "List::push",
            Some(crate::ast::ReceiverKind::RefMut),
            &["T"],
            Some("()"),
            builtin_value("List::push", |args, _span| {
                match (args.first(), args.get(1)) {
                    (Some(Value::Struct { name, fields }), Some(val)) if name == "List" => {
                        if let Some(Value::Array(arr)) = fields.get("inner") {
                            arr.borrow_mut().push(val.clone());
                            Ok(Value::Unit)
                        } else {
                            Err(MetelError::internal("List::push: missing inner field"))
                        }
                    }
                    _ => Err(MetelError::internal("List::push: expected (List, T)")),
                }
            }),
        )
    );

    register_inherent!(
        "List",
        "pop",
        method(
            "List::pop",
            Some(crate::ast::ReceiverKind::RefMut),
            &[],
            Some("Perhaps<T>"),
            builtin_value("List::pop", |args, span| {
                match args.first() {
                    Some(Value::Struct { name, fields }) if name == "List" => {
                        if let Some(Value::Array(arr)) = fields.get("inner") {
                            match arr.borrow_mut().pop() {
                                Some(val) => {
                                    let mut f = std::collections::HashMap::new();
                                    f.insert("value".to_string(), val);
                                    Ok(Value::Enum {
                                        name: "Perhaps".to_string(),
                                        variant: "Some".to_string(),
                                        fields: f,
                                    })
                                }
                                None => Ok(Value::Enum {
                                    name: "Perhaps".to_string(),
                                    variant: "None".to_string(),
                                    fields: std::collections::HashMap::new(),
                                }),
                            }
                        } else {
                            Err(MetelError::internal("List::pop: missing inner field"))
                        }
                    }
                    _ => Err(MetelError::panic(
                        RuntimeErrorCode::R0009,
                        "List::pop: expected List",
                        span,
                    )),
                }
            }),
        )
    );

    register_inherent!(
        "List",
        "len",
        method(
            "List::len",
            Some(crate::ast::ReceiverKind::Value),
            &[],
            Some("i64"),
            builtin_value("List::len", |args, _span| {
                match args.first() {
                    Some(Value::Struct { name, fields }) if name == "List" => {
                        if let Some(Value::Array(arr)) = fields.get("inner") {
                            Ok(Value::I64(arr.borrow().len() as i64))
                        } else {
                            Err(MetelError::internal("List::len: missing inner field"))
                        }
                    }
                    _ => Err(MetelError::internal("List::len: expected List")),
                }
            }),
        )
    );

    register_inherent!(
        "List",
        "get",
        method(
            "List::get",
            Some(crate::ast::ReceiverKind::Value),
            &["i64"],
            Some("Perhaps<T>"),
            builtin_value("List::get", |args, _span| {
                match (args.first(), args.get(1)) {
                    (Some(Value::Struct { name, fields }), Some(Value::I64(idx)))
                        if name == "List" =>
                    {
                        if let Some(Value::Array(arr)) = fields.get("inner") {
                            match arr.borrow().get(*idx as usize).cloned() {
                                Some(val) => {
                                    let mut f = std::collections::HashMap::new();
                                    f.insert("value".to_string(), val);
                                    Ok(Value::Enum {
                                        name: "Perhaps".to_string(),
                                        variant: "Some".to_string(),
                                        fields: f,
                                    })
                                }
                                None => Ok(Value::Enum {
                                    name: "Perhaps".to_string(),
                                    variant: "None".to_string(),
                                    fields: std::collections::HashMap::new(),
                                }),
                            }
                        } else {
                            Err(MetelError::internal("List::get: missing inner field"))
                        }
                    }
                    _ => Err(MetelError::internal("List::get: expected (List, i64)")),
                }
            }),
        )
    );

    register_inherent!(
        "List",
        "as_slice",
        method(
            "List::as_slice",
            Some(crate::ast::ReceiverKind::Value),
            &[],
            Some("T[]"),
            builtin_value("List::as_slice", |args, _span| {
                match args.first() {
                    Some(Value::Struct { name, fields }) if name == "List" => {
                        if let Some(arr) = fields.get("inner") {
                            Ok(arr.clone())
                        } else {
                            Err(MetelError::internal("List::as_slice: missing inner field"))
                        }
                    }
                    _ => Err(MetelError::internal("List::as_slice: expected List")),
                }
            }),
        )
    );

    // clock / assert / assert_msg / dbg are registered by
    // register_core_natives_from_embedded above.
}

pub(super) fn runtime_registry() -> RuntimeRegistry {
    let mut runtime = RuntimeRegistry::new();
    register_builtins(&mut runtime);
    runtime
}

#[cfg(test)]
mod native_tests {
    use super::*;

    #[test]
    fn every_native_key_has_a_host_impl() {
        // Coverage: each NativeKey must resolve to a registered host
        // implementation. This replaces the old free_function_names() parity
        // check as the single source of truth for native dispatch (METEL-182).
        for key in NativeKey::ALL {
            let callable = native_host_impl(*key);
            match callable {
                RuntimeCallable::Intrinsic { label, .. } => {
                    assert!(!label.is_empty(), "native impl for {key:?} has empty label");
                }
                _ => panic!("native impl for {key:?} is not an intrinsic"),
            }
        }
    }
}
