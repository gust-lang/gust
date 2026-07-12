use super::Value;
use crate::types::Type;

/// Derive a concrete `Type` from a runtime `Value`.
///
/// Used during construction-at-call-time for generic function bodies: the caller
/// maps each argument value to its type, then uses those types to instantiate the
/// function's `TypeScheme` and build the `Substitution` for `ConstructCtx`.
///
/// Limitations:
/// - Generic structs/enums: type parameters are not recoverable from runtime values,
///   so `Named(name, [])` is returned. The construction pass must unify against the
///   scheme and fill in the parameters from context.
/// - Closures: the concrete function type is not stored in the runtime callable, so
///   `Fun([], Box::new(Unit))` is returned as a placeholder.
pub(super) fn value_to_type(value: &Value) -> Type {
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
        Value::Tuple(elems) => Type::Tuple(elems.iter().map(value_to_type).collect()),
        Value::Array(rc) => {
            let borrowed = rc.borrow();
            let elem_ty = borrowed.first().map_or(Type::Unit, value_to_type);
            Type::Array(Box::new(elem_ty))
        }
        Value::Struct { name, .. } | Value::Enum { name, .. } => Type::Named(name.clone(), vec![]),
        Value::Callable(callable) => match callable {
            super::RuntimeCallable::Closure(rc) => rc
                .fun_type
                .clone()
                .unwrap_or_else(|| Type::Fun(vec![], Box::new(Type::Unit))),
            super::RuntimeCallable::Intrinsic { .. } => Type::Fun(vec![], Box::new(Type::Unit)),
        },
        Value::Reference(rc) => Type::Reference(Box::new(value_to_type(&rc.borrow()))),
        Value::MutReference(rc) => Type::MutReference(Box::new(value_to_type(&rc.borrow()))),
        Value::MutFieldReference { root, path } => {
            // Approximate: read the leaf type from the current root value.
            let root_val = root.borrow();
            let mut cur_type = value_to_type(&root_val);
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
