//! Semantics tests for `distinct Name = Representation`: what it refuses, what
//! it lets through, and the two crossings that are the whole of its surface.

use super::{analyze_text, codes, diagnostics};
use kira_semantics_model::Type;

#[test]
fn a_distinct_type_is_built_by_calling_it_and_read_through_raw() {
    assert!(
        diagnostics(
            "distinct TabId = U32
             function tabIndex(id: TabId) -> U32 { return id.raw }
             @Main function main() {
                 let id: TabId = TabId(U32(1))
                 print(Int(tabIndex(id)))
                 return
             }"
        )
        .is_empty()
    );
}

/// The refusal the type exists for, in both directions and against a sibling.
#[test]
fn a_distinct_type_is_assignable_to_nothing_but_itself() {
    // Its representation does not reach it.
    assert_eq!(
        codes(
            "distinct TabId = U32
             function takeTab(id: TabId) -> U32 { return id.raw }
             @Main function main() { let raw: U32 = U32(1) print(Int(takeTab(raw))) return }"
        ),
        vec!["KSEM063"]
    );
    // It does not reach its representation.
    assert_eq!(
        codes(
            "distinct TabId = U32
             function takeWord(word: U32) -> U32 { return word }
             @Main function main() { let id = TabId(U32(1)) print(Int(takeWord(id))) return }"
        ),
        vec!["KSEM063"]
    );
    // And a second distinct type over the same representation does not either,
    // which is the case a `type` alias cannot express at all.
    assert_eq!(
        codes(
            "distinct TabId = U32
             distinct BookmarkId = U32
             function takeTab(id: TabId) -> U32 { return id.raw }
             @Main function main() {
                 let bookmark = BookmarkId(U32(1))
                 print(Int(takeTab(bookmark)))
                 return
             }"
        ),
        vec!["KSEM063"]
    );
}

/// `.raw` reads the representation, at the representation's exact width.
#[test]
fn raw_reads_the_representation_at_its_own_width() {
    let program = analyze_text(
        "distinct TabId = U32
         @Main function main() { let word = TabId(U32(7)).raw print(Int(word)) return }",
    );
    let main = program
        .functions
        .iter()
        .find(|function| function.is_main)
        .expect("an entrypoint");
    let word = main
        .locals
        .iter()
        .find(|local| local.name == "word")
        .expect("the binding `.raw` produced");
    assert_eq!(
        word.ty,
        Type::Int(kira_semantics_model::IntSpelling::U32),
        "`.raw` on a `distinct TabId = U32` reads a `U32`, not a bare `Int`"
    );
}

/// A distinct type has one member and it is `raw`; everything else is named.
#[test]
fn a_distinct_type_has_no_member_but_raw() {
    assert_eq!(
        codes(
            "distinct TabId = U32
             @Main function main() { let id = TabId(U32(1)) print(Int(id.value)) return }"
        ),
        vec!["KSEM349"]
    );
}

/// `.raw` is a read, not a place: a distinct type has no storage to write into.
#[test]
fn raw_cannot_be_assigned_through() {
    assert!(
        !diagnostics(
            "distinct TabId = U32
             @Main function main() { var id = TabId(U32(1)) id.raw = U32(2) return }"
        )
        .is_empty(),
        "writing through `.raw` names storage a distinct type does not have"
    );
}

/// A distinct type is not its representation, so it does not print as one.
#[test]
fn a_distinct_value_is_not_printable() {
    assert_eq!(
        codes(
            "distinct TabId = U32
             @Main function main() { print(TabId(U32(1))) return }"
        ),
        vec!["KSEM081"]
    );
    // Its representation is, which is what `.raw` is for.
    assert!(
        diagnostics(
            "distinct TabId = U32
             @Main function main() { print(Int(TabId(U32(1)).raw)) return }"
        )
        .is_empty()
    );
}

/// Construction takes one value of the representation, and says so when it does
/// not get one.
#[test]
fn construction_takes_one_value_of_the_representation() {
    assert_eq!(
        codes(
            "distinct TabId = U32
             @Main function main() { let id = TabId() print(Int(id.raw)) return }"
        ),
        vec!["KSEM347"]
    );
    assert_eq!(
        codes(
            "distinct TabId = U32
             @Main function main() { let id = TabId(\"one\") print(Int(id.raw)) return }"
        ),
        vec!["KSEM348"]
    );
    // An integer literal reaches any integer width, so it reaches the
    // representation too: `TabId(1)` is `TabId(U32(1))` written shorter.
    assert!(
        diagnostics(
            "distinct TabId = U32
             @Main function main() { let id = TabId(1) print(Int(id.raw)) return }"
        )
        .is_empty()
    );
}

/// Two ids compare, and comparing one to anything else does not.
#[test]
fn equality_is_the_operator_surface_a_distinct_type_has() {
    assert!(
        diagnostics(
            "distinct TabId = U32
             @Main function main() {
                 let a = TabId(U32(1))
                 let b = TabId(U32(2))
                 if a == b { print(1) }
                 if a != b { print(2) }
                 return
             }"
        )
        .is_empty()
    );
    // Arithmetic is refused: an id is named, not counted.
    assert_eq!(
        codes(
            "distinct TabId = U32
             @Main function main() {
                 let a = TabId(U32(1))
                 let b = TabId(U32(2))
                 print(Int((a + b).raw))
                 return
             }"
        ),
        vec!["KSEM071"]
    );
    // And so is comparing one to its representation.
    assert_eq!(
        codes(
            "distinct TabId = U32
             @Main function main() {
                 let a = TabId(U32(1))
                 if a == U32(1) { print(1) }
                 return
             }"
        ),
        vec!["KSEM071"]
    );
}

/// Every fixed-width numeric primitive backs a distinct type, plus `Int`,
/// `Float`, `Bool`, and `RawPtr`.
///
/// The list is the language's whole scalar vocabulary. There is no `I64` and no
/// `F64` to test: `Int` *is* the 64-bit signed integer and `Float` the 64-bit
/// float, so those two spellings cover the widths a `distinct` declaration
/// would otherwise write them as.
#[test]
fn every_scalar_word_backs_a_distinct_type() {
    for representation in [
        "I8", "U8", "I16", "U16", "I32", "U32", "U64", "F32", "Int", "Float", "Bool", "RawPtr",
    ] {
        let source = format!(
            "distinct Handle = {representation}
             function widen(value: Handle) -> Handle {{ return value }}
             @Main function main() {{ return }}"
        );
        assert!(
            diagnostics(&source).is_empty(),
            "`distinct Handle = {representation}` should declare a type",
        );
    }
}

/// A representation that owns storage is refused by name, with the fix.
#[test]
fn a_representation_that_owns_storage_is_refused() {
    assert_eq!(
        codes(
            "distinct Label = String
             @Main function main() { return }"
        ),
        vec!["KSEM345"]
    );
    assert_eq!(
        codes(
            "struct Point { var x: Int }
             distinct Anchor = Point
             @Main function main() { return }"
        ),
        vec!["KSEM345"]
    );
}

/// A chain of distinct types is a chain of *types*, each its own.
#[test]
fn a_chain_declares_a_new_type_at_every_link() {
    assert!(
        diagnostics(
            "distinct Millis = U64
             distinct Ticks = Millis
             function elapsed(value: Ticks) -> U64 { return value.raw }
             @Main function main() { print(Int(elapsed(Ticks(U64(4))))) return }"
        )
        .is_empty()
    );
    // `.raw` reads through the whole chain to the scalar, so a link is not a
    // second unwrap the reader has to remember.
    assert_eq!(
        codes(
            "distinct Millis = U64
             distinct Ticks = Millis
             function elapsed(value: Millis) -> U64 { return value.raw }
             @Main function main() { print(Int(elapsed(Ticks(U64(4))))) return }"
        ),
        vec!["KSEM063"]
    );
}

/// A cycle terminates and is reported once, exactly as an alias cycle is.
#[test]
fn a_cycle_is_reported_once() {
    assert_eq!(
        codes(
            "distinct A = B
             distinct B = A
             function use(value: A) -> A { return value }
             @Main function main() { return }"
        ),
        vec!["KSEM346"]
    );
}

/// A name may mean one thing.
#[test]
fn a_distinct_type_may_not_claim_a_taken_name() {
    assert_eq!(
        codes(
            "distinct Int = U32
             @Main function main() { return }"
        ),
        vec!["KSEM130"]
    );
    assert_eq!(
        codes(
            "struct TabId { var value: Int }
             distinct TabId = U32
             @Main function main() { return }"
        ),
        vec!["KSEM130"]
    );
    assert_eq!(
        codes(
            "type TabId = U32
             distinct TabId = U32
             @Main function main() { return }"
        ),
        vec!["KSEM130"]
    );
}

/// A distinct type is a type, so it goes wherever a type goes.
#[test]
fn a_distinct_type_reaches_every_type_position() {
    assert!(
        diagnostics(
            "distinct TabId = U32
             struct Tab { var id: TabId }
             function first(ids: borrow [TabId]) -> TabId { return ids[0] }
             @Main function main() {
                 var ids: [TabId] = []
                 ids.append(TabId(U32(1)))
                 let tab = Tab { id: first(ids) }
                 print(Int(tab.id.raw))
                 return
             }"
        )
        .is_empty()
    );
    // And `[TabId]` is not `[U32]`, which is the property an alias cannot give.
    assert_eq!(
        codes(
            "distinct TabId = U32
             function total(words: borrow [U32]) -> Int { return words.count }
             @Main function main() {
                 var ids: [TabId] = []
                 print(total(ids))
                 return
             }"
        ),
        vec!["KSEM063"]
    );
}

/// A distinct type is one scalar word, so it copies without `move`.
#[test]
fn a_distinct_value_copies_without_move() {
    assert!(
        diagnostics(
            "distinct TabId = U32
             function consume(id: TabId) -> U32 { return id.raw }
             @Main function main() {
                 let id = TabId(U32(1))
                 print(Int(consume(id)))
                 print(Int(consume(id)))
                 return
             }"
        )
        .is_empty()
    );
}

/// A distinct type crosses the C seam as the representation it is.
#[test]
fn a_distinct_type_crosses_the_c_seam_as_its_representation() {
    let text = "distinct TabId = U32\n\
                @FFI.Extern { library: tabs, symbol: tab_close, abi: c }\n\
                function tabClose(id: TabId) -> U32\n\
                @Main function main() { print(Int(tabClose(TabId(U32(1))))) return }";
    assert!(diagnostics(text).is_empty(), "{:?}", codes(text));
    // Nominality survives the seam: a foreign parameter written `TabId` still
    // takes a `TabId` and not the `U32` it lowers to.
    let raw = "distinct TabId = U32\n\
               @FFI.Extern { library: tabs, symbol: tab_close, abi: c }\n\
               function tabClose(id: TabId) -> U32\n\
               @Main function main() { print(Int(tabClose(U32(1)))) return }";
    assert_eq!(codes(raw), vec!["KSEM183"]);
}

/// `Option<Value>` is an ordinary generic enum, so it instantiates over one.
#[test]
fn option_instantiates_over_a_distinct_type() {
    let text = "enum Option<Value> { Some(Value) None }
                distinct TabId = U32
                function open(selected: borrow Option<TabId>) -> U32 {
                    match selected {
                        Some(id) -> return id.raw
                        None -> return U32(0)
                    }
                }
                @Main function main() {
                    let found: Option<TabId> = .Some(TabId(U32(3)))
                    let missing: Option<TabId> = .None
                    print(Int(open(found)) + Int(open(missing)))
                    return
                }";
    assert!(diagnostics(text).is_empty(), "{:?}", codes(text));
}
