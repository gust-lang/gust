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
    /// `std::core::string_len` — number of characters in a string.
    StdCoreStringLen,
    /// `std::core::string_concat` — concatenate two strings.
    StdCoreStringConcat,
    /// `Display::to_string` for every displayable primitive — the host formats
    /// the receiver by its runtime value, so one key serves all 13 impls.
    StdCoreToString,
    /// `i8::from(numeric)` — convert any numeric value to i8.
    StdCoreI8From,
    /// `i16::from(numeric)` — convert any numeric value to i16.
    StdCoreI16From,
    /// `i32::from(numeric)` — convert any numeric value to i32.
    StdCoreI32From,
    /// `i64::from(numeric)` — convert any numeric value to i64.
    StdCoreI64From,
    /// `u8::from(numeric)` — convert any numeric value to u8.
    StdCoreU8From,
    /// `u16::from(numeric)` — convert any numeric value to u16.
    StdCoreU16From,
    /// `u32::from(numeric | Char)` — convert a numeric value or a Char code
    /// point to u32.
    StdCoreU32From,
    /// `u64::from(numeric)` — convert any numeric value to u64.
    StdCoreU64From,
    /// `f32::from(numeric)` — convert any numeric value to f32.
    StdCoreF32From,
    /// `f64::from(numeric)` — convert any numeric value to f64.
    StdCoreF64From,
    /// `Char::from(u32)` — code point to Char; panics on invalid scalars.
    StdCoreCharFrom,
    /// `List::new()` — empty list.
    StdCoreListNew,
    /// `List::from(T[])` — list with a copy of the array's elements.
    StdCoreListFrom,
    /// `List::push(&mut self, T)` — append an element.
    StdCoreListPush,
    /// `List::pop(&mut self) -> Perhaps<T>` — remove and return the last element.
    StdCoreListPop,
    /// `List::len(self) -> i64` — number of elements.
    StdCoreListLen,
    /// `List::get(self, i64) -> Perhaps<T>` — element at an index.
    StdCoreListGet,
    /// `List::as_slice(self) -> T[]` — the backing array.
    StdCoreListAsSlice,
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
            ["std", "core", "string_len"] => NativeKey::StdCoreStringLen,
            ["std", "core", "string_concat"] => NativeKey::StdCoreStringConcat,
            ["std", "core", "to_string"] => NativeKey::StdCoreToString,
            ["std", "core", "i8_from"] => NativeKey::StdCoreI8From,
            ["std", "core", "i16_from"] => NativeKey::StdCoreI16From,
            ["std", "core", "i32_from"] => NativeKey::StdCoreI32From,
            ["std", "core", "i64_from"] => NativeKey::StdCoreI64From,
            ["std", "core", "u8_from"] => NativeKey::StdCoreU8From,
            ["std", "core", "u16_from"] => NativeKey::StdCoreU16From,
            ["std", "core", "u32_from"] => NativeKey::StdCoreU32From,
            ["std", "core", "u64_from"] => NativeKey::StdCoreU64From,
            ["std", "core", "f32_from"] => NativeKey::StdCoreF32From,
            ["std", "core", "f64_from"] => NativeKey::StdCoreF64From,
            ["std", "core", "char_from"] => NativeKey::StdCoreCharFrom,
            ["std", "core", "list_new"] => NativeKey::StdCoreListNew,
            ["std", "core", "list_from"] => NativeKey::StdCoreListFrom,
            ["std", "core", "list_push"] => NativeKey::StdCoreListPush,
            ["std", "core", "list_pop"] => NativeKey::StdCoreListPop,
            ["std", "core", "list_len"] => NativeKey::StdCoreListLen,
            ["std", "core", "list_get"] => NativeKey::StdCoreListGet,
            ["std", "core", "list_as_slice"] => NativeKey::StdCoreListAsSlice,
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
            NativeKey::StdCoreStringLen => "@std.core.string_len",
            NativeKey::StdCoreStringConcat => "@std.core.string_concat",
            NativeKey::StdCoreToString => "@std.core.to_string",
            NativeKey::StdCoreI8From => "@std.core.i8_from",
            NativeKey::StdCoreI16From => "@std.core.i16_from",
            NativeKey::StdCoreI32From => "@std.core.i32_from",
            NativeKey::StdCoreI64From => "@std.core.i64_from",
            NativeKey::StdCoreU8From => "@std.core.u8_from",
            NativeKey::StdCoreU16From => "@std.core.u16_from",
            NativeKey::StdCoreU32From => "@std.core.u32_from",
            NativeKey::StdCoreU64From => "@std.core.u64_from",
            NativeKey::StdCoreF32From => "@std.core.f32_from",
            NativeKey::StdCoreF64From => "@std.core.f64_from",
            NativeKey::StdCoreCharFrom => "@std.core.char_from",
            NativeKey::StdCoreListNew => "@std.core.list_new",
            NativeKey::StdCoreListFrom => "@std.core.list_from",
            NativeKey::StdCoreListPush => "@std.core.list_push",
            NativeKey::StdCoreListPop => "@std.core.list_pop",
            NativeKey::StdCoreListLen => "@std.core.list_len",
            NativeKey::StdCoreListGet => "@std.core.list_get",
            NativeKey::StdCoreListAsSlice => "@std.core.list_as_slice",
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
        NativeKey::StdCoreStringLen,
        NativeKey::StdCoreStringConcat,
        NativeKey::StdCoreToString,
        NativeKey::StdCoreI8From,
        NativeKey::StdCoreI16From,
        NativeKey::StdCoreI32From,
        NativeKey::StdCoreI64From,
        NativeKey::StdCoreU8From,
        NativeKey::StdCoreU16From,
        NativeKey::StdCoreU32From,
        NativeKey::StdCoreU64From,
        NativeKey::StdCoreF32From,
        NativeKey::StdCoreF64From,
        NativeKey::StdCoreCharFrom,
        NativeKey::StdCoreListNew,
        NativeKey::StdCoreListFrom,
        NativeKey::StdCoreListPush,
        NativeKey::StdCoreListPop,
        NativeKey::StdCoreListLen,
        NativeKey::StdCoreListGet,
        NativeKey::StdCoreListAsSlice,
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
