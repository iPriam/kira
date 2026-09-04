//! The export ABI's marker, and the symbol family it guards.
//!
//! A Kira library built for the native engine exports one C-ABI trampoline per
//! `@Export`, all sharing the uniform shape the hybrid seam already load-tests.
//! Those symbols resolve **by name**, which is the failure mode worth designing
//! against: an archive built under an older contract still links, and the
//! program then calls the old code through the new ABI.
//!
//! [`RUNTIME_ABI_MARKER`](kira_runtime_abi::RUNTIME_ABI_MARKER) answers that for
//! the `kira_rt_*` helpers by baking the version into a symbol name. This module
//! applies the same trick one level up: a library defines
//! `kira_lib_<library>_abi_1`, and the generated wrapper *calls* it from
//! `load()`. The call does nothing — the function is empty and free — and that
//! is the point. A stale archive does not define this version's marker, so the
//! consumer's **link** fails naming the marker instead of the process
//! misbehaving later.
//!
//! # Two versions, deliberately separate
//!
//! [`EXPORT_ABI_VERSION`] is not
//! [`RUNTIME_ABI_VERSION`](kira_runtime_abi::RUNTIME_ABI_VERSION). They version
//! different contracts and move independently: the runtime version covers what a
//! `kira_rt_*` helper does and owns, this one covers what a `kira_lib_*`
//! trampoline's arguments mean. A library carries both markers, and either one
//! going stale fails the same way.
//!
//! # Where these names are used
//!
//! The VM engine needs none of this: it verifies data rather than symbols (see
//! [`Library::verify`](crate::Library)), because a `.kbc` embedded in a wrapper
//! crate has no link step to fail. The names here are the native engine's half
//! of the guard, defined now so both engines' guards are stated in one place and
//! pinned by one test.

/// The version of the `kira_lib_*` export contract.
///
/// Bump this — and with it the marker symbol every library defines — on any
/// change to what a trampoline's arguments mean, what it owns, or who frees
/// what across the export boundary.
pub const EXPORT_ABI_VERSION: u32 = 1;

/// The prefix every symbol a Kira library exports carries.
///
/// Disjoint by construction from `kira_x_`, the prefix the opposite direction
/// (a Rust crate imported *into* Kira) claims, so both features can live in one
/// process without a name ever colliding.
pub const EXPORT_SYMBOL_PREFIX: &str = "kira_lib_";

/// The marker symbol a library defines and its generated wrapper calls.
///
/// `library` is the package name; the result is `kira_lib_<library>_abi_1` for
/// [`EXPORT_ABI_VERSION`] 1.
pub fn export_abi_marker(library: &str) -> String {
    format!("{EXPORT_SYMBOL_PREFIX}{library}_abi_{EXPORT_ABI_VERSION}")
}

/// The symbol one exported function's trampoline is emitted under.
///
/// `export` is the **consumer-facing** name — the snake_cased spelling the
/// module's export table carries, not the name the Kira author wrote. That
/// mapping is derived once in the frontend and travels in the artifact; this
/// function never re-derives it, so there is only ever one answer to what an
/// export is called.
pub fn export_symbol(library: &str, export: &str) -> String {
    format!("{EXPORT_SYMBOL_PREFIX}{library}_{export}")
}

/// The symbol the synthesized destructor for one exported class is emitted
/// under.
///
/// `class` is the consumer-facing spelling, for the same reason
/// [`export_symbol`] takes one.
pub fn class_drop_symbol(library: &str, class: &str) -> String {
    format!("{EXPORT_SYMBOL_PREFIX}{library}_drop_{class}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The marker's spelling *is* the guard, so it is pinned rather than
    /// recomputed: a test that rebuilt the name from the same constants would
    /// pass through any rename, which is exactly the change that must not pass
    /// silently.
    #[test]
    fn the_marker_symbol_is_the_name_it_has_always_been() {
        assert_eq!(EXPORT_ABI_VERSION, 1);
        assert_eq!(
            export_abi_marker("uifoundation"),
            "kira_lib_uifoundation_abi_1"
        );
    }

    #[test]
    fn every_exported_symbol_is_the_name_it_has_always_been() {
        assert_eq!(
            export_symbol("uifoundation", "make_button"),
            "kira_lib_uifoundation_make_button"
        );
        assert_eq!(
            class_drop_symbol("uifoundation", "button"),
            "kira_lib_uifoundation_drop_button"
        );
    }

    #[test]
    fn every_symbol_this_family_mints_carries_the_prefix() {
        for symbol in [
            export_abi_marker("uifoundation"),
            export_symbol("uifoundation", "make_button"),
            class_drop_symbol("uifoundation", "button"),
        ] {
            assert!(
                symbol.starts_with(EXPORT_SYMBOL_PREFIX),
                "`{symbol}` escapes the prefix that keeps the two directions apart"
            );
            assert!(
                !symbol.starts_with("kira_x_"),
                "`{symbol}` collides with the import direction's namespace"
            );
        }
    }

    /// The two versions guard different contracts and are free to differ. This
    /// records that they are read from different constants, so a future bump of
    /// one never silently moves the other.
    #[test]
    fn the_export_version_is_not_the_runtime_version() {
        assert_eq!(kira_runtime_abi::RUNTIME_ABI_VERSION, 15);
        assert_eq!(EXPORT_ABI_VERSION, 1);
    }
}
