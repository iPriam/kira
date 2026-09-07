//! The `RawPtr` surface: the null a program spells, and comparing two pointer
//! words.

use super::*;
use kira_semantics_model::hir::{HirBinaryOp, HirExpr};

/// Whether the program holds the null-pointer constant.
fn has_null(text: &str) -> bool {
    analyze_text(text)
        .exprs
        .iter()
        .any(|(_, expr)| matches!(expr, HirExpr::RawPtrNull))
}

#[test]
fn raw_ptr_null_is_the_null_pointer_constant() {
    let source = "@Main function main() { let p: RawPtr = RawPtr.null \
                  print(rawPointerWord(p)) return }";
    assert!(diagnostics(source).is_empty(), "{:?}", diagnostics(source));
    assert!(has_null(source));
}

#[test]
fn raw_ptr_has_no_member_but_null() {
    let source = "@Main function main() { let p: RawPtr = RawPtr.zero return }";
    assert_eq!(codes(source), vec!["KSEM368"]);
}

#[test]
fn a_local_named_raw_ptr_still_shadows_the_type() {
    // The rule every builtin-type name follows here: a binding the reader can
    // see beats a type they have to look up.
    let source = r#"
struct Holder { var null: Int }

@Main function main() {
    let RawPtr = Holder { null: 7 }
    print(RawPtr.null)
    return
}
"#;
    assert!(diagnostics(source).is_empty(), "{:?}", diagnostics(source));
    assert!(!has_null(source));
}

#[test]
fn two_pointers_compare_as_the_words_they_are() {
    let source = r#"
@FFI.Struct { layout: c }
struct State { var tag: I32 }

@FFI.Pointer { target: State, ownership: borrowed }
struct StatePtr {}

@FFI.Extern { library: fixture, symbol: ffi_state, abi: c }
function state() -> StatePtr

@Main function main() {
    print(state() == RawPtr.null)
    print(state() != state())
    print(RawPtr.null == RawPtr.null)
    return
}
"#;
    assert!(diagnostics(source).is_empty(), "{:?}", diagnostics(source));
    // The comparison is an integer one over the two words, so no backend
    // learns that pointers can be compared.
    let program = analyze_text(source);
    let comparisons = program
        .exprs
        .iter()
        .filter(|(_, expr)| {
            matches!(
                expr,
                HirExpr::Binary {
                    op: HirBinaryOp::EqInt | HirBinaryOp::NeInt,
                    ..
                }
            )
        })
        .count();
    assert_eq!(comparisons, 3);
}

#[test]
fn a_distinct_pointer_compares_only_to_itself() {
    let source = r#"
distinct Adapter = RawPtr
distinct Surface = RawPtr

@Main function main() {
    let a = Adapter(RawPtr.null)
    let b = Adapter(RawPtr.null)
    print(a == b)
    return
}
"#;
    assert!(diagnostics(source).is_empty(), "{:?}", diagnostics(source));

    let crossed = r#"
distinct Adapter = RawPtr
distinct Surface = RawPtr

@Main function main() {
    let a = Adapter(RawPtr.null)
    let s = Surface(RawPtr.null)
    print(a == s)
    return
}
"#;
    assert_eq!(codes(crossed), vec!["KSEM071"]);
}

#[test]
fn a_pointer_has_no_ordering() {
    // C only defines `<` between two pointers into one object, and Kira has no
    // arithmetic that could produce such a pair.
    let source = r#"
@Main function main() {
    let p: RawPtr = RawPtr.null
    print(p < RawPtr.null)
    return
}
"#;
    assert_eq!(codes(source), vec!["KSEM071"]);
}

#[test]
fn a_pointer_does_not_compare_to_its_word() {
    // `RawPtr` is not an integer; reading the word is what `rawPointerWord`
    // is for, and it is written rather than implied.
    let source = r#"
@Main function main() {
    let p: RawPtr = RawPtr.null
    print(p == 0)
    return
}
"#;
    assert_eq!(codes(source), vec!["KSEM071"]);
}
