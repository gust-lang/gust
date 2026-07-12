/// Resolved types — produced by the type checker, consumed by the evaluator.
/// No type variables exist here; generics have been monomorphised.
#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    Boolean,
    Str,
    Char,
    Unit,
    /// The bottom type `!`. Produced by expressions that never return (infinite
    /// loops with no reachable `break`, `return`, `panic!`). Coerces to any type.
    Never,
    // ── Sized integer types ───────────────────────────────────────────────────
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
    // ── Sized float types ─────────────────────────────────────────────────────
    F32,
    F64,
    // ─────────────────────────────────────────────────────────────────────────
    Tuple(Vec<Type>),
    Array(Box<Type>),
    SizedArray(Box<Type>, u64),
    Reference(Box<Type>),
    MutReference(Box<Type>),
    Fun(Vec<Type>, Box<Type>),
    /// A named type (struct, enum) with concrete type arguments after monomorphisation.
    Named(String, Vec<Type>),
}

impl Type {
    /// Returns true if this is any integer type (signed or unsigned, any width).
    #[must_use]
    pub fn is_integer(&self) -> bool {
        matches!(
            self,
            Type::I64
                | Type::I8
                | Type::I16
                | Type::I32
                | Type::U8
                | Type::U16
                | Type::U32
                | Type::U64
        )
    }

    /// Returns true if this is any float type.
    #[must_use]
    pub fn is_float(&self) -> bool {
        matches!(self, Type::F64 | Type::F32)
    }

    /// Returns true if this is any numeric type (integer or float).
    #[must_use]
    pub fn is_numeric(&self) -> bool {
        self.is_integer() || self.is_float()
    }
}

impl std::fmt::Display for Type {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Type::I64 => write!(f, "i64"),
            Type::F64 => write!(f, "f64"),
            Type::Boolean => write!(f, "boolean"),
            Type::Str => write!(f, "String"),
            Type::Char => write!(f, "Char"),
            Type::Unit => write!(f, "()"),
            Type::Never => write!(f, "!"),
            Type::I8 => write!(f, "i8"),
            Type::I16 => write!(f, "i16"),
            Type::I32 => write!(f, "i32"),
            Type::U8 => write!(f, "u8"),
            Type::U16 => write!(f, "u16"),
            Type::U32 => write!(f, "u32"),
            Type::U64 => write!(f, "u64"),
            Type::F32 => write!(f, "f32"),
            Type::Tuple(ts) => {
                write!(f, "(")?;
                for (i, t) in ts.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{t}")?;
                }
                write!(f, ")")
            }
            Type::Array(t) => write!(f, "{t}[]"),
            Type::SizedArray(t, n) => write!(f, "[{t}; {n}]"),
            Type::Reference(t) => write!(f, "&{t}"),
            Type::MutReference(t) => write!(f, "&mut {t}"),
            Type::Fun(params, ret) => {
                write!(f, "(")?;
                for (i, t) in params.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{t}")?;
                }
                write!(f, ") -> {ret}")
            }
            Type::Named(name, args) => {
                write!(f, "{name}")?;
                if !args.is_empty() {
                    write!(f, "<")?;
                    for (i, a) in args.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{a}")?;
                    }
                    write!(f, ">")?;
                }
                Ok(())
            }
        }
    }
}
