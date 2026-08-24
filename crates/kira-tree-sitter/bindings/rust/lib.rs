//! The tree-sitter grammar for Kira, as a linkable Rust crate.
//!
//! The grammar itself lives beside this binding in the package layout the
//! tree-sitter CLI works on — `grammar.js` and the generated `src/parser.c` at
//! the crate root — so the CLI, the node binding, and the editor extensions
//! keep reading the layout they expect while Rust consumers link this crate.
//!
//! The one export is the language's raw function pointer word. A consumer
//! constructs its own `tree_sitter::Language` from it, which is what keeps
//! this crate free of a `tree-sitter` runtime dependency and therefore out of
//! every version-lockstep problem between the grammar and its consumers.

use std::ffi::c_void;

unsafe extern "C" {
    /// The entry point the generated parser exports.
    fn tree_sitter_kira() -> *const c_void;
}

/// The Kira grammar's language pointer, as tree-sitter's C ABI hands it out.
///
/// Never null: the generated parser returns the address of its static
/// language descriptor. The caller wraps it in its own tree-sitter runtime's
/// `Language` type; the descriptor lives for the whole process, so no
/// lifetime rides along.
pub fn language_pointer() -> *const c_void {
    // SAFETY: the generated `tree_sitter_kira` takes nothing, reads nothing
    // but its own statics, and returns a pointer to static storage.
    unsafe { tree_sitter_kira() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_language_descriptor_exists() {
        assert!(!language_pointer().is_null());
    }
}
