//! Macro failures through the real binary: what is reported, and what is not.
//!
//! A broken macro used to bury its own error: an unclosed `quote` left every
//! later `#{` in place, and each one was reported as an unexpected character
//! far from the brace that never closed. These prove the failure names itself.

use crate::{kira, write_program};

/// Runs `kira check` on a one-file program and returns its output.
fn check_program(entry: &str) -> std::process::Output {
    let path = write_program(entry, &[]);
    let output = kira(&["check", path.to_str().unwrap()]);
    let _ = std::fs::remove_dir_all(path.parent().expect("program directory"));
    output
}

#[test]
fn an_unclosed_macro_body_names_itself_rather_than_every_splice_after_it() {
    let output = check_program(
        "comptime macro Broken {\n\
         kind { derive }\n\
         appliesTo { struct }\n\
         expand(target: Declaration) -> Syntax {\n\
             return quote {\n\
                 function broken() -> Int {\n\
                     return 1\n\
         }\n\
         }\n\
         @Main function main() { print(1) return }\n",
    );
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("KMAC012"), "{stderr}");
    assert!(stderr.contains("unclosed"), "{stderr}");
    assert!(
        !stderr.contains("KLEX001"),
        "no surviving-splice noise: {stderr}"
    );
}

#[test]
fn an_identifier_built_from_a_non_name_is_refused_where_the_text_is() {
    let output = check_program(
        "comptime macro BadName {\n\
         kind { function }\n\
         expand(input: Syntax) -> Syntax {\n\
             return quote { #{Identifier(\"has space\")} }\n\
         }\n\
         }\n\
         @Main function main() { print(BadName!(x)) return }\n\
         function x() -> Int { return 1 }",
    );
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("KMAC013"), "{stderr}");
}

#[test]
fn an_array_of_a_generic_is_refused_with_what_it_holds() {
    let output = check_program(
        "import Foundation\n\
         @Derive(Serializable)\n\
         struct Boxed {\n\
             var items: [Option<Int>]\n\
         }\n\
         @Main function main() { print(1) return }",
    );
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("array elements"), "{stderr}");
}
