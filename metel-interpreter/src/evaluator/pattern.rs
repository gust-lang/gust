use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::ast::{Literal, Pattern};

use super::Value;

// Float literal patterns (e.g. `1.0 => ...`) match by exact IEEE-754 equality,
// matching the same exact-comparison semantics as `==` (see `eval_binop`) --
// not a bug, so not rewritten to an epsilon comparison.
#[allow(clippy::float_cmp)]
// Exhaustive match over every Pattern/Value combination; splitting it up would
// scatter one coherent dispatch table across many small functions with no
// real gain in clarity.
#[allow(clippy::too_many_lines)]
pub(super) fn match_pattern(
    pattern: &Pattern,
    value: &Value,
    out: &mut HashMap<String, Value>,
) -> bool {
    match pattern {
        Pattern::Wildcard(_) => true,

        Pattern::Literal(lit, _) => match (lit, value) {
            (Literal::Int(a), Value::I64(b)) => a == b,
            (Literal::Float(a), Value::F64(b)) => a == b,
            (Literal::Char(a), Value::Char(b)) => a == b,
            (Literal::Boolean(a), Value::Boolean(b)) => a == b,
            (Literal::Str(a), Value::Str(b)) => a == b,
            (Literal::Unit, Value::Unit) => true,
            (Literal::None, Value::Enum { name, variant, .. })
                if name == "Perhaps" && variant == "None" =>
            {
                true
            }
            (Literal::SizedInt { value: a, kind }, v) => {
                use crate::ast::IntKind;
                match kind {
                    IntKind::I8 => matches!(v, Value::I8(b)  if *b == *a as i8),
                    IntKind::I16 => matches!(v, Value::I16(b) if *b == *a as i16),
                    IntKind::I32 => matches!(v, Value::I32(b) if *b == *a as i32),
                    IntKind::I64 => matches!(v, Value::I64(b) if *b == *a as i64),
                    IntKind::U8 => matches!(v, Value::U8(b)  if *b == *a as u8),
                    IntKind::U16 => matches!(v, Value::U16(b) if *b == *a as u16),
                    IntKind::U32 => matches!(v, Value::U32(b) if *b == *a as u32),
                    IntKind::U64 => matches!(v, Value::U64(b) if *b == *a as u64),
                }
            }
            (Literal::SizedFloat { value: a, kind }, v) => {
                use crate::ast::FloatKind;
                match kind {
                    FloatKind::F32 => matches!(v, Value::F32(b) if *b == *a as f32),
                    FloatKind::F64 => matches!(v, Value::F64(b) if *b == *a),
                }
            }
            _ => false,
        },

        Pattern::Binding(name, _) => {
            out.insert(name.clone(), value.clone());
            true
        }

        Pattern::Tuple(sub_patterns, _) => match value {
            Value::Tuple(elems) if elems.len() == sub_patterns.len() => sub_patterns
                .iter()
                .zip(elems.iter())
                .all(|(p, v)| match_pattern(p, v, out)),
            _ => false,
        },

        Pattern::EnumVariant { path, fields, .. } => {
            let type_name = if path.len() >= 2 {
                path[path.len() - 2].as_str()
            } else {
                ""
            };
            let variant_name = path.last().map_or("", String::as_str);
            match value {
                Value::Enum {
                    name,
                    variant,
                    fields: enum_fields,
                    ..
                } if name == type_name && variant == variant_name => {
                    for field_name in fields {
                        match enum_fields.get(field_name) {
                            Some(v) => {
                                out.insert(field_name.clone(), v.clone());
                            }
                            None => return false,
                        }
                    }
                    true
                }
                _ => false,
            }
        }

        Pattern::Array { elems, rest, .. } => {
            match value {
                Value::Array(rc) => {
                    let arr = rc.borrow();
                    if rest.is_none() {
                        // Exact match: array must have exactly `elems.len()` elements.
                        if arr.len() != elems.len() {
                            return false;
                        }
                    } else {
                        // Rest pattern: array must have at least `elems.len()` elements.
                        if arr.len() < elems.len() {
                            return false;
                        }
                    }
                    // Match explicit element patterns.
                    for (pat, val) in elems.iter().zip(arr.iter()) {
                        if !match_pattern(pat, val, out) {
                            return false;
                        }
                    }
                    // Bind rest to the remaining tail.
                    if let Some(rest_name) = rest {
                        let tail: Vec<Value> = arr[elems.len()..].to_vec();
                        out.insert(rest_name.clone(), Value::Array(Rc::new(RefCell::new(tail))));
                    }
                    true
                }
                _ => false,
            }
        }
    }
}
