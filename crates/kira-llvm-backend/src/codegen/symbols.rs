//! The native symbol every Kira function is emitted under.
//!
//! Two naming schemes, and the difference between them is the whole point:
//!
//! - [`symbol_name`] names a function's *body*, which only this module's own
//!   code calls. It is an implementation detail.
//! - [`trampoline_name`] names the fixed-shape entry the *host* calls to reach
//!   a `@Native` function. That one is a wire contract: the hybrid manifest
//!   records it as the function's exported symbol, and the host resolves it out
//!   of the shared library by exactly this name.

/// The symbol of the trampoline the host calls to reach native function `index`.
///
/// A wire contract with the hybrid manifest, which records this name as the
/// function's exported symbol.
pub(crate) fn trampoline_name(index: usize) -> String {
    format!("kira_native_fn_{index}")
}

/// The native symbol for Kira function `index`.
///
/// The index makes every symbol unique even when two Kira functions share a
/// name, and keeps the symbol stable against source reordering within a program.
pub(super) fn symbol_name(index: usize, name: &str) -> String {
    let sanitized: String = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect();
    format!("kira_fn_{index}_{sanitized}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbols_are_unique_per_function_and_sanitized() {
        assert_eq!(symbol_name(0, "main"), "kira_fn_0_main");
        assert_eq!(symbol_name(3, "fib"), "kira_fn_3_fib");
        // Two functions sharing a name never collide on a symbol.
        assert_ne!(symbol_name(1, "helper"), symbol_name(2, "helper"));
        // Anything a linker could not carry is replaced, not passed through.
        assert_eq!(symbol_name(0, "odd name!"), "kira_fn_0_odd_name_");
    }

    /// The trampoline name is what the manifest records and the host resolves,
    /// so it is spelled once, here, and never rebuilt at a call site.
    #[test]
    fn trampolines_are_named_per_function_id() {
        assert_eq!(trampoline_name(0), "kira_native_fn_0");
        assert_eq!(trampoline_name(7), "kira_native_fn_7");
    }
}
