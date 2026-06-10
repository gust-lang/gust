//! Stdlib-only native host bindings (METEL-182).
//!
//! A `native(@std.core.print) fun print(x: String);` declaration binds a stdlib
//! function to a host (Rust) implementation. The surface id (`@std.core.print`)
//! is lowered to a closed [`NativeKey`] so dispatch never depends on the surface
//! spelling, and coverage between declared native functions and registered host
//! implementations can be checked exhaustively.
//!
//! The enum is intentionally closed: every variant must have exactly one stdlib
//! declaration and exactly one host implementation. Third-party native providers
//! are an FFI-era concern, out of scope here.

/// A host-backed standard-library function, identified independently of its
/// surface name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NativeKey {
    /// `std::core::print` — write a value's Display form to stdout, no newline.
    StdCorePrint,
    /// `std::core::println` — write a value's Display form to stdout with newline.
    StdCorePrintln,
    /// `std::core::dbg` — debug-print a value to stderr and return it unchanged.
    StdCoreDbg,
    /// `std::core::assert` — panic if the condition is false.
    StdCoreAssert,
    /// `std::core::assert_msg` — panic with a message if the condition is false.
    StdCoreAssertMsg,
    /// `std::core::clock` — milliseconds since the Unix epoch.
    StdCoreClock,
}

impl NativeKey {
    /// Lower a dotted surface id (`["std","core","print"]`) to a [`NativeKey`].
    /// Returns `None` for an unknown id — the caller reports it as an error so
    /// the closed enum stays the single source of truth for host bindings.
    pub fn from_path(path: &[String]) -> Option<NativeKey> {
        let segments: Vec<&str> = path.iter().map(String::as_str).collect();
        let key = match segments.as_slice() {
            ["std", "core", "print"] => NativeKey::StdCorePrint,
            ["std", "core", "println"] => NativeKey::StdCorePrintln,
            ["std", "core", "dbg"] => NativeKey::StdCoreDbg,
            ["std", "core", "assert"] => NativeKey::StdCoreAssert,
            ["std", "core", "assert_msg"] => NativeKey::StdCoreAssertMsg,
            ["std", "core", "clock"] => NativeKey::StdCoreClock,
            _ => return None,
        };
        Some(key)
    }

    /// The surface id this key lowers from, for diagnostics.
    pub fn surface_id(&self) -> &'static str {
        match self {
            NativeKey::StdCorePrint => "@std.core.print",
            NativeKey::StdCorePrintln => "@std.core.println",
            NativeKey::StdCoreDbg => "@std.core.dbg",
            NativeKey::StdCoreAssert => "@std.core.assert",
            NativeKey::StdCoreAssertMsg => "@std.core.assert_msg",
            NativeKey::StdCoreClock => "@std.core.clock",
        }
    }

    /// Every variant — used by the coverage check that asserts each has a host
    /// implementation registered.
    pub const ALL: &'static [NativeKey] = &[
        NativeKey::StdCorePrint,
        NativeKey::StdCorePrintln,
        NativeKey::StdCoreDbg,
        NativeKey::StdCoreAssert,
        NativeKey::StdCoreAssertMsg,
        NativeKey::StdCoreClock,
    ];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_known_paths() {
        for key in NativeKey::ALL {
            let id = key.surface_id().trim_start_matches('@');
            let path: Vec<String> = id.split('.').map(str::to_string).collect();
            assert_eq!(NativeKey::from_path(&path), Some(*key), "for {id}");
        }
    }

    #[test]
    fn unknown_path_is_none() {
        assert_eq!(
            NativeKey::from_path(&["std".into(), "core".into(), "nope".into()]),
            None
        );
        assert_eq!(NativeKey::from_path(&["foo".into()]), None);
    }
}
