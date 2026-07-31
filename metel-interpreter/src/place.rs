//! Places: the syntactic locations a program can name.
//!
//! A place is a binding root plus a path of projections — `x`, `x.f`, `x.f.g`,
//! `*p`, or "reached through a dynamic index". This module is deliberately
//! **analysis-neutral**, per RFC-0071 §9b: it carries no move-specific state and
//! makes no move-specific assumption, so that borrow checking can later run a
//! second analysis over the *same* places without rebuilding them and without the
//! two analyses disagreeing about partial moves.
//!
//! It lives at the crate root, not inside `move_check`, for that reason
//! (adr-0045). Policy lives with each analysis, not here. That a move out of an
//! [`Projection::OpaqueIndex`] element is rejected, or that a move through a
//! [`Projection::Deref`] needs a reborrow, are facts about *moves*; this module
//! only says such a place exists and how it relates to its prefixes.

use crate::ast::UnaryOp;
use crate::typed_ast::{TypedExpr, TypedPlace};

/// One step of a path from a binding root to a sub-location.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Projection {
    /// A named field: `.f`.
    Field(String),
    /// A tuple element: `.0`.
    TupleIndex(usize),
    /// An element reached through a dynamic index: `[i]`. Which element is not
    /// known statically, so two `OpaqueIndex` steps into the same sequence are
    /// the same place.
    OpaqueIndex,
    /// The pointee of a reference: `*p`.
    Deref,
}

/// A binding root plus the path of projections that reaches a sub-location of it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Place {
    root: String,
    projections: Vec<Projection>,
}

impl Place {
    #[must_use]
    pub fn new(root: String) -> Self {
        Self {
            root,
            projections: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_projection(mut self, projection: Projection) -> Self {
        self.projections.push(projection);
        self
    }

    #[must_use]
    pub fn root(&self) -> &str {
        &self.root
    }

    #[must_use]
    pub fn projections(&self) -> &[Projection] {
        &self.projections
    }

    /// Whether this place is `other` itself or an ancestor of it, so that
    /// anything true of this place is true of `other`.
    #[must_use]
    pub fn is_prefix_of(&self, other: &Self) -> bool {
        self.root == other.root
            && self.projections.len() <= other.projections.len()
            && self
                .projections
                .iter()
                .zip(other.projections.iter())
                .all(|(left, right)| left == right)
    }
}

/// Renders a place the way it would be written in source: `x.f`, `xs[_]` for a
/// dynamic index, `(*p).f` for a projection through a reference.
impl std::fmt::Display for Place {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut rendered = self.root.clone();
        for projection in &self.projections {
            match projection {
                Projection::Field(field) => {
                    rendered.push('.');
                    rendered.push_str(field);
                }
                Projection::TupleIndex(index) => {
                    rendered.push('.');
                    rendered.push_str(&index.to_string());
                }
                Projection::OpaqueIndex => rendered.push_str("[_]"),
                // A deref reads leftwards, so it wraps what has been built so far.
                Projection::Deref => rendered = format!("(*{rendered})"),
            }
        }
        f.write_str(&rendered)
    }
}

/// The place an expression names, if it names one.
///
/// Returns `None` for an expression that produces a fresh value rather than
/// naming an existing location (a call, a literal, an arithmetic result).
#[must_use]
pub fn from_expr(expr: &TypedExpr) -> Option<Place> {
    match expr {
        TypedExpr::Ident(name, _, _) => Some(Place::new(name.clone())),
        TypedExpr::UnaryOp(UnaryOp::Deref, object, _, _) => {
            Some(from_expr(object)?.with_projection(Projection::Deref))
        }
        TypedExpr::FieldAccess { object, field, .. } => {
            Some(from_expr(object)?.with_projection(Projection::Field(field.clone())))
        }
        TypedExpr::TupleAccess { object, index, .. } => {
            Some(from_expr(object)?.with_projection(Projection::TupleIndex(*index)))
        }
        TypedExpr::Index { object, .. } => {
            Some(from_expr(object)?.with_projection(Projection::OpaqueIndex))
        }
        _ => None,
    }
}

/// The place an assignment target names.
#[must_use]
pub fn from_typed_place(place: &TypedPlace) -> Option<Place> {
    match place {
        TypedPlace::Ident(name, _) => Some(Place::new(name.clone())),
        TypedPlace::Field { object, field, .. } => Some(
            from_typed_place(object)?.with_projection(Projection::Field(field.clone())),
        ),
        TypedPlace::Tuple { object, index, .. } => {
            Some(from_typed_place(object)?.with_projection(Projection::TupleIndex(*index)))
        }
        TypedPlace::Index { object, .. } => {
            Some(from_typed_place(object)?.with_projection(Projection::OpaqueIndex))
        }
        // A deref target holds the *expression* being dereferenced, not a
        // nested place, so this is where the two constructors meet.
        TypedPlace::Deref { object, .. } => {
            Some(from_expr(object)?.with_projection(Projection::Deref))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Place, Projection};

    #[test]
    fn prefix_matches_exact_place() {
        let place = Place::new("x".to_string()).with_projection(Projection::Field("a".to_string()));
        assert!(place.is_prefix_of(&place));
    }

    #[test]
    fn prefix_matches_descendant_place() {
        let left = Place::new("x".to_string()).with_projection(Projection::Field("a".to_string()));
        let right = left
            .clone()
            .with_projection(Projection::Field("b".to_string()));
        assert!(left.is_prefix_of(&right));
    }

    #[test]
    fn prefix_rejects_sibling_place() {
        let left = Place::new("x".to_string()).with_projection(Projection::Field("a".to_string()));
        let right = Place::new("x".to_string()).with_projection(Projection::Field("b".to_string()));
        assert!(!left.is_prefix_of(&right));
    }

    #[test]
    fn opaque_index_is_a_real_projection() {
        let left = Place::new("xs".to_string()).with_projection(Projection::OpaqueIndex);
        let right = left
            .clone()
            .with_projection(Projection::Field("len".to_string()));
        assert!(left.is_prefix_of(&right));
    }

    #[test]
    fn deref_is_a_real_projection() {
        let left = Place::new("p".to_string()).with_projection(Projection::Deref);
        let right = left
            .clone()
            .with_projection(Projection::Field("name".to_string()));
        assert!(left.is_prefix_of(&right));
        assert_eq!(left.projections(), &[Projection::Deref]);
    }

    #[test]
    fn deref_is_distinct_from_the_reference_itself() {
        let reference = Place::new("p".to_string());
        let pointee = reference.clone().with_projection(Projection::Deref);
        assert_ne!(reference, pointee);
        assert!(reference.is_prefix_of(&pointee));
        assert!(!pointee.is_prefix_of(&reference));
    }
}
