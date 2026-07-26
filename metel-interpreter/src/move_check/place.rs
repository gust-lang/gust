use crate::typed_ast::{TypedExpr, TypedPlace};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Projection {
    Field(String),
    TupleIndex(usize),
    OpaqueIndex,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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

#[must_use]
pub fn from_expr(expr: &TypedExpr) -> Option<Place> {
    match expr {
        TypedExpr::Ident(name, _, _) => Some(Place::new(name.clone())),
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
        TypedPlace::Deref { .. } => None,
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
}
