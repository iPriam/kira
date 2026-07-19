//! The bundled Foundation on the Web.
//!
//! Foundation resolves in the loader, so by the time this backend sees a
//! program it is one `IrProgram` carrying no trace of where its functions came
//! from — there is nothing here for the wasm lowering to have got wrong. What
//! these cases add over the parity suite is that the *shipped* Foundation
//! source compiles and runs on both widths: the file committed in this
//! repository is read off disk through the same discovery a `kirac` uses, not
//! retyped into the test, so a change to Foundation that wasm cannot carry
//! fails here.

use crate::assert_module_parity;

/// Foundation's real source, as the loader finds it.
///
/// `None` when no bundle is discoverable — a checkout that somehow has no
/// committed Foundation, which is a broken tree rather than a failing feature.
fn foundation_source() -> Option<String> {
    let roots = kira_program_graph::bundled::bundled_roots();
    let root = roots.first()?;
    std::fs::read_to_string(root.source_dir().join("Foundation.kira")).ok()
}

#[test]
fn the_shipped_foundation_runs_on_both_widths() {
    let Some(text) = foundation_source() else {
        return;
    };
    assert_module_parity(
        "import Foundation\n\
         @Main function main() { printLine(\"from Foundation\") return }",
        &[("Foundation", text.as_str())],
    );
}

#[test]
fn a_qualified_foundation_call_runs_on_both_widths() {
    let Some(text) = foundation_source() else {
        return;
    };
    assert_module_parity(
        "import Foundation\n\
         @Main function main() { Foundation.printLine(\"qualified\") return }",
        &[("Foundation", text.as_str())],
    );
}

/// A borrowed String crossing into Foundation and being printed there is the
/// one thing `printLine` actually does, and the bump allocator this backend
/// runs on is where a borrow that was really a copy would show up.
#[test]
fn a_borrowed_string_reaches_foundation_on_both_widths() {
    let Some(text) = foundation_source() else {
        return;
    };
    assert_module_parity(
        "import Foundation\n\
         @Main function main() { let greeting = \"hello\" + \", web\" \
         printLine(greeting) printLine(greeting) return }",
        &[("Foundation", text.as_str())],
    );
}
