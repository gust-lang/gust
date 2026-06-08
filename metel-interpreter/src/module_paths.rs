use crate::ast::PathRoot;

/// Compute the absolute module path prefix for a given [`PathRoot`],
/// relative to the current module path. See ADR-0023.
pub fn resolve_path_root(root: &PathRoot, current: &[String]) -> Vec<String> {
    match root {
        PathRoot::Root => vec![],
        PathRoot::Std => vec!["std".to_string()],
        PathRoot::Self_ => current.to_vec(),
        PathRoot::Super => {
            if current.is_empty() {
                vec![]
            } else {
                current[..current.len() - 1].to_vec()
            }
        }
        PathRoot::Name(n) => {
            let mut path = current.to_vec();
            path.push(n.clone());
            path
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn root_always_empty() {
        assert_eq!(resolve_path_root(&PathRoot::Root, &s(&["a", "b"])), s(&[]));
        assert_eq!(resolve_path_root(&PathRoot::Root, &s(&[])), s(&[]));
    }

    #[test]
    fn std_always_std() {
        assert_eq!(
            resolve_path_root(&PathRoot::Std, &s(&["a", "b"])),
            s(&["std"])
        );
        assert_eq!(resolve_path_root(&PathRoot::Std, &s(&[])), s(&["std"]));
    }

    #[test]
    fn self_returns_current() {
        assert_eq!(
            resolve_path_root(&PathRoot::Self_, &s(&["a", "b"])),
            s(&["a", "b"])
        );
        assert_eq!(resolve_path_root(&PathRoot::Self_, &s(&[])), s(&[]));
    }

    #[test]
    fn super_drops_last_segment() {
        assert_eq!(
            resolve_path_root(&PathRoot::Super, &s(&["a", "b", "c"])),
            s(&["a", "b"])
        );
        assert_eq!(resolve_path_root(&PathRoot::Super, &s(&["a"])), s(&[]));
        assert_eq!(resolve_path_root(&PathRoot::Super, &s(&[])), s(&[]));
    }

    #[test]
    fn name_appends_to_current() {
        assert_eq!(
            resolve_path_root(&PathRoot::Name("foo".to_string()), &s(&["a", "b"])),
            s(&["a", "b", "foo"])
        );
        assert_eq!(
            resolve_path_root(&PathRoot::Name("foo".to_string()), &s(&[])),
            s(&["foo"])
        );
    }
}
