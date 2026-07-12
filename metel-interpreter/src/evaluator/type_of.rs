use std::collections::HashMap;

use super::Value;
use crate::ast::Span;
use crate::types::Type;
use crate::typeinference::TypeDefinitionRegistry;

/// Derive a concrete `Type` from a runtime `Value`.
///
/// Used during construction-at-call-time for generic function bodies: the caller
/// maps each argument value to its type, then uses those types to instantiate the
/// function's `TypeScheme` and build the `Substitution` for `ConstructCtx`.
///
/// Limitations:
/// - Closures: the concrete function type is not stored in the runtime callable, so
///   `Fun([], Box::new(Unit))` is returned as a placeholder.
///
/// Generic structs/enums (issue #267): the runtime value itself carries no type
/// argument info (`Wrapper { value: 5 }`'s only intrinsic type tag is bare
/// `Named("Wrapper", [])`), so `registry` and `span` are used to recover them —
/// see `typechecker::infer_named_type_args`'s own doc comment for the mechanism.
pub(super) fn value_to_type(value: &Value, registry: &TypeDefinitionRegistry, span: &Span) -> Type {
    let go = |v: &Value| value_to_type(v, registry, span);
    match value {
        Value::I64(_) => Type::I64,
        Value::F64(_) => Type::F64,
        Value::Char(_) => Type::Char,
        Value::Boolean(_) => Type::Boolean,
        Value::Str(_) => Type::Str,
        Value::Unit => Type::Unit,
        Value::I8(_) => Type::I8,
        Value::I16(_) => Type::I16,
        Value::I32(_) => Type::I32,
        Value::U8(_) => Type::U8,
        Value::U16(_) => Type::U16,
        Value::U32(_) => Type::U32,
        Value::U64(_) => Type::U64,
        Value::F32(_) => Type::F32,
        Value::Tuple(elems) => Type::Tuple(elems.iter().map(go).collect()),
        Value::Array(rc) => {
            let borrowed = rc.borrow();
            let elem_ty = borrowed.first().map_or(Type::Unit, go);
            Type::Array(Box::new(elem_ty))
        }
        Value::Struct { name, fields, .. } => {
            let field_types: HashMap<String, Type> =
                fields.iter().map(|(k, v)| (k.clone(), go(v))).collect();
            let args = crate::typechecker::infer_named_type_args(
                name,
                None,
                &field_types,
                registry,
                span,
            );
            Type::Named(name.clone(), args)
        }
        Value::Enum {
            name,
            variant,
            fields,
            ..
        } => {
            let field_types: HashMap<String, Type> =
                fields.iter().map(|(k, v)| (k.clone(), go(v))).collect();
            let args = crate::typechecker::infer_named_type_args(
                name,
                Some(variant),
                &field_types,
                registry,
                span,
            );
            Type::Named(name.clone(), args)
        }
        Value::Callable(callable) => match callable {
            super::RuntimeCallable::Closure(rc) => rc
                .fun_type
                .clone()
                .unwrap_or_else(|| Type::Fun(vec![], Box::new(Type::Unit))),
            super::RuntimeCallable::Intrinsic { .. } => Type::Fun(vec![], Box::new(Type::Unit)),
        },
        Value::Reference(rc) => Type::Reference(Box::new(go(&rc.borrow()))),
        Value::MutReference(rc) => Type::MutReference(Box::new(go(&rc.borrow()))),
        Value::MutFieldReference { root, path } => {
            // Approximate: read the leaf type from the current root value.
            let root_val = root.borrow();
            let mut cur_type = go(&root_val);
            for seg in path {
                cur_type = match (seg, cur_type) {
                    (super::PathSegment::Field(f), Type::Named(name, _)) => {
                        Type::Named(format!("{name}.{f}"), vec![])
                    }
                    (super::PathSegment::TupleIndex(_) | super::PathSegment::ArrayIndex(_), t) => t,
                    _ => Type::Unit,
                };
            }
            Type::MutReference(Box::new(cur_type))
        }
    }
}
