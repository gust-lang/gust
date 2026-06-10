//! Standard-library sources embedded into the binary at build time (METEL-181).
//!
//! `build.rs` scans `stdlib/**/*.mtl` and generates `EMBEDDED_STDLIB`, a table of
//! `(module_path_segments, source)` pairs. The module loader serves these
//! through `EmbeddedStdlibProvider` so `std::…` modules need no on-disk files.

include!(concat!(env!("OUT_DIR"), "/stdlib_embedded.rs"));

/// Embedded source for a logical stdlib module path (e.g. `["std", "core"]`),
/// or `None` if no embedded module matches.
pub fn lookup(module_path: &[String]) -> Option<&'static str> {
    EMBEDDED_STDLIB
        .iter()
        .find(|(segs, _)| {
            segs.len() == module_path.len()
                && segs.iter().zip(module_path).all(|(a, b)| *a == b.as_str())
        })
        .map(|(_, src)| *src)
}

/// Every embedded stdlib module path. Used by the loader to synthesize the
/// stdlib modules into the module graph ahead of user code.
pub fn module_paths() -> Vec<Vec<String>> {
    EMBEDDED_STDLIB
        .iter()
        .map(|(segs, _)| segs.iter().map(|s| s.to_string()).collect())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_module_is_embedded() {
        let core = vec!["std".to_string(), "core".to_string()];
        let src = lookup(&core).expect("std::core must be embedded");
        assert!(src.contains("native(@std.core.print)"), "core.mtl content");
        assert!(module_paths().contains(&core));
    }

    #[test]
    fn unknown_path_is_none() {
        assert!(lookup(&["std".to_string(), "nope".to_string()]).is_none());
        assert!(lookup(&["user".to_string()]).is_none());
    }
}
