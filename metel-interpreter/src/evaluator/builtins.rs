use crate::error::{MetelError, RuntimeErrorCode};

use super::display::{format_value, value_to_display_string};
use super::{
    RuntimeCallable, RuntimeMethod, RuntimeRegistry, RuntimeSignature, RuntimeTypePattern,
    RuntimeTypeRef, Value,
};

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

// `Display::to_string` for every displayable primitive: one host fn formats the
// receiver by its runtime value, so all 13 std::core impls share one NativeKey.
fn native_to_string(args: Vec<Value>, span: &crate::ast::Span) -> Result<Value, MetelError> {
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

// Numeric `From` conversions. The source type is encoded in the value itself,
// so one host fn per TARGET covers its whole From<…> impl family. Integer
// targets truncate through i128, float targets convert through f64 — the same
// semantics as the per-pair builtins these replace.
macro_rules! native_int_from {
    ($fn_name:ident, $label:literal, $out:expr) => {
        fn $fn_name(args: Vec<Value>, _span: &crate::ast::Span) -> Result<Value, MetelError> {
            match args.first().and_then(numeric_as_i128) {
                Some(n) => Ok($out(n)),
                None => Err(MetelError::internal(concat!(
                    $label,
                    "::from: expected a numeric argument"
                ))),
            }
        }
    };
}
macro_rules! native_float_from {
    ($fn_name:ident, $label:literal, $out:expr) => {
        fn $fn_name(args: Vec<Value>, _span: &crate::ast::Span) -> Result<Value, MetelError> {
            match args.first().and_then(numeric_as_f64_val) {
                Some(f) => Ok($out(f)),
                None => Err(MetelError::internal(concat!(
                    $label,
                    "::from: expected a numeric argument"
                ))),
            }
        }
    };
}
native_int_from!(native_i8_from, "i8", |n: i128| Value::I8(n as i8));
native_int_from!(native_i16_from, "i16", |n: i128| Value::I16(n as i16));
native_int_from!(native_i32_from, "i32", |n: i128| Value::I32(n as i32));
native_int_from!(native_i64_from, "i64", |n: i128| Value::I64(n as i64));
native_int_from!(native_u8_from, "u8", |n: i128| Value::U8(n as u8));
native_int_from!(native_u16_from, "u16", |n: i128| Value::U16(n as u16));
native_int_from!(native_u64_from, "u64", |n: i128| Value::U64(n as u64));
native_float_from!(native_f32_from, "f32", |f: f64| Value::F32(f as f32));
native_float_from!(native_f64_from, "f64", |f: f64| Value::F64(f));

// u32 additionally accepts a Char (its Unicode code point).
fn native_u32_from(args: Vec<Value>, _span: &crate::ast::Span) -> Result<Value, MetelError> {
    match args.first() {
        Some(Value::Char(c)) => Ok(Value::U32(*c as u32)),
        Some(v) => match numeric_as_i128(v) {
            Some(n) => Ok(Value::U32(n as u32)),
            None => Err(MetelError::internal(
                "u32::from: expected a numeric or Char argument",
            )),
        },
        None => Err(MetelError::internal("u32::from: expected an argument")),
    }
}

fn native_char_from(args: Vec<Value>, span: &crate::ast::Span) -> Result<Value, MetelError> {
    match args.first() {
        Some(Value::U32(n)) => char::from_u32(*n).map(Value::Char).ok_or_else(|| {
            MetelError::panic(
                RuntimeErrorCode::R0009,
                format!("u32 value {n} is not a valid Unicode scalar"),
                span,
            )
        }),
        _ => Err(MetelError::internal("Char::from: expected a u32 argument")),
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
            NativeKey::StdCoreToString => ("Display::to_string", native_to_string),
            NativeKey::StdCoreI8From => ("i8::from", native_i8_from),
            NativeKey::StdCoreI16From => ("i16::from", native_i16_from),
            NativeKey::StdCoreI32From => ("i32::from", native_i32_from),
            NativeKey::StdCoreI64From => ("i64::from", native_i64_from),
            NativeKey::StdCoreU8From => ("u8::from", native_u8_from),
            NativeKey::StdCoreU16From => ("u16::from", native_u16_from),
            NativeKey::StdCoreU32From => ("u32::from", native_u32_from),
            NativeKey::StdCoreU64From => ("u64::from", native_u64_from),
            NativeKey::StdCoreF32From => ("f32::from", native_f32_from),
            NativeKey::StdCoreF64From => ("f64::from", native_f64_from),
            NativeKey::StdCoreCharFrom => ("Char::from", native_char_from),
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
    fn key_for(binding: &crate::ast::NativeBinding) -> NativeKey {
        NativeKey::from_path(&binding.key_path).unwrap_or_else(|| {
            panic!(
                "embedded std::core declares unknown native binding @{}",
                binding.key_path.join(".")
            )
        })
    }
    for decl in &program.decls {
        match decl {
            crate::ast::Decl::Fun(fun) => {
                let Some(binding) = &fun.native else { continue };
                let key = key_for(binding);
                runtime.register_std_core_value(
                    fun.name.clone(),
                    Value::Callable(native_host_impl(key)),
                );
            }
            crate::ast::Decl::Impl(ib) => {
                let crate::ast::TypeExpr::Named(target_name, _) = &ib.target_type else {
                    continue;
                };
                for method in &ib.methods {
                    // Only native methods are derivable here; a Metel-bodied
                    // core impl method would need elaboration, which this
                    // single-program seeding path does not run.
                    let Some(binding) = &method.native else { continue };
                    let key = key_for(binding);
                    let receiver = method.params.first().and_then(|p| p.receiver.clone());
                    let params: Vec<RuntimeTypeRef> = method
                        .params
                        .iter()
                        .filter(|p| p.receiver.is_none())
                        .map(|p| {
                            p.type_ann.as_ref().map(super::runtime_type_ref).expect(
                                "native declarations are fully annotated (enforced by native_fun_ty)",
                            )
                        })
                        .collect();
                    let runtime_method = RuntimeMethod {
                        label: format!("{target_name}::{}", method.name),
                        receiver,
                        signature: RuntimeSignature {
                            params,
                            ret: method.return_type.as_ref().map(super::runtime_type_ref),
                        },
                        body: native_host_impl(key),
                    };
                    if let Some(aspect_name) = &ib.aspect_name {
                        let type_args = ib
                            .aspect_type_args
                            .iter()
                            .map(super::runtime_type_key)
                            .collect();
                        runtime.register_aspect_method(
                            target_name,
                            aspect_name,
                            None,
                            type_args,
                            &method.name,
                            runtime_method,
                        );
                    } else if runtime_method.receiver.is_none() {
                        runtime.register_type_value(target_name, &method.name, runtime_method);
                    } else {
                        runtime.register_inherent_method(target_name, &method.name, runtime_method);
                    }
                }
            }
            _ => {}
        }
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
    // std::core free functions (print/println/…) and the native impl methods
    // (Display::to_string on primitives, the numeric From cross-product,
    // Char ↔ u32) — all derived from the embedded core.mtl declarations.
    register_core_natives_from_embedded(runtime);

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
