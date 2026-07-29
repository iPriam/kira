//! The compiler intrinsic has one fixed signature.
//!
//! `[String]` in and `[String]` out, with no overloading and no inference — the
//! same discipline the file-system intrinsics follow, and for the same reason:
//! every backend has to know the operand types of a `Compiler` instruction
//! without the instruction carrying them.

use super::{codes, diagnostics};

#[test]
fn the_compiler_intrinsic_type_checks_with_its_own_result() {
    assert!(
        diagnostics(
            r#"
@Main function main() {
    var request: [String] = ["App"]
    var answer: [String] = kcCheckPackages(request)
    return
}
"#
        )
        .is_empty()
    );
}

#[test]
fn the_intrinsic_refuses_the_wrong_number_of_arguments() {
    assert_eq!(
        codes("@Main function main() { kcCheckPackages(); return }"),
        vec!["KSEM252"]
    );
    assert_eq!(
        codes(r#"@Main function main() { var r: [String] = []; kcCheckPackages(r, r); return }"#),
        vec!["KSEM252"]
    );
}

#[test]
fn the_intrinsic_refuses_an_argument_of_the_wrong_type() {
    assert_eq!(
        codes(r#"@Main function main() { kcCheckPackages("App"); return }"#),
        vec!["KSEM253"]
    );
    assert_eq!(
        codes("@Main function main() { var r: [Int] = [1]; kcCheckPackages(r); return }"),
        vec!["KSEM253"]
    );
}
