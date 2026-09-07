//! The file-system intrinsics have one fixed signature each.
//!
//! No overloading and no inference: a call either matches the declared shape or
//! is refused. That is what lets every backend know the operand types of a
//! `FileSystem` instruction without carrying them.

use super::{codes, diagnostics};

const USES: &str = r#"
@Main function main() {
    var bytes: [U8] = [1, 2]
    fsWriteBytes("a.bin", bytes)
    var read: [U8] = fsReadRange("a.bin", 0, 2)
    var names: [String] = fsListDirectory(".")
    var text: String = fsReadText("a.bin")
    var size: U64 = fsFileSize("a.bin")
    fsWriteText("a.txt", text)
    fsRenamePath("a.txt", "b.txt")
    fsIsDirectory(".")
    fsMakeDirectory("d")
    fsRemovePath("d")
    fsFileExists("b.txt")
    fsPathExists("b.txt")
    return
}
"#;

#[test]
fn every_file_system_intrinsic_type_checks_with_its_own_result() {
    assert!(diagnostics(USES).is_empty());
}

#[test]
fn an_intrinsic_refuses_the_wrong_number_of_arguments() {
    assert_eq!(
        codes("@Main function main() { fsReadText() return }"),
        vec!["KSEM252"]
    );
    assert_eq!(
        codes(r#"@Main function main() { fsReadRange("a", 0) return }"#),
        vec!["KSEM252"]
    );
    assert_eq!(
        codes(r#"@Main function main() { fsFileSize("a", "b") return }"#),
        vec!["KSEM252"]
    );
}

#[test]
fn an_intrinsic_refuses_an_argument_of_the_wrong_type() {
    assert_eq!(
        codes("@Main function main() { fsReadText(7) return }"),
        vec!["KSEM253"]
    );
    assert_eq!(
        codes(r#"@Main function main() { fsReadRange("a", "b", 2) return }"#),
        vec!["KSEM253"]
    );
    assert_eq!(
        codes(r#"@Main function main() { fsWriteBytes("a", "not bytes") return }"#),
        vec!["KSEM253"]
    );
}

/// A byte array is `[U8]` exactly. `[Int]` is a different type, and the width
/// rule is what says so — the wildcard runs between spellings of a scalar, not
/// between two array types the table interned separately.
#[test]
fn writing_bytes_requires_a_byte_array() {
    assert_eq!(
        codes(
            r#"
@Main function main() {
    var wide: [Int] = [1, 2]
    fsWriteBytes("a.bin", wide)
    return
}
"#,
        ),
        vec!["KSEM253"]
    );
}

/// An intrinsic takes a label and ignores it, like every other call surface.
#[test]
fn an_intrinsic_accepts_and_ignores_an_argument_label() {
    assert!(diagnostics(r#"@Main function main() { fsReadText(path: "a") return }"#).is_empty());
}
