//! The `@FFI.Extern` / `@FFI.Address` declaration itself: the annotation
//! block, the names it may take, and every refusal that reads the declaration
//! rather than the types in it.

use super::*;

// ----- structural / parser boundary ---------------------------------------

#[test]
fn a_bodyless_ordinary_function_is_a_parse_error() {
    let text = "@Main function main() { return }\nfunction f() -> I32";
    assert!(
        codes(text).iter().any(|code| code == "KPAR055"),
        "{:?}",
        codes(text)
    );
}

#[test]
fn an_extern_with_a_body_is_a_parse_error() {
    let text = "@Main function main() { return }\n\
                @FFI.Extern { library: l, symbol: s, abi: c } function f() -> I32 { return 1 }";
    assert!(
        codes(text).iter().any(|code| code == "KPAR054"),
        "{:?}",
        codes(text)
    );
}

#[test]
fn grouped_extern_library_declarations_share_library_and_resolve_normally() {
    let text = r#"
extern library ffimath {
    function add(a: I32, b: I32) -> I32
    function negate(value: I32) -> I32
}
@Main function main() {
    print(add(1, negate(2)))
    return
}
"#;
    assert!(codes(text).is_empty(), "{:?}", codes(text));
}

// ----- annotation block meaning -------------------------------------------

#[test]
fn a_missing_required_field_is_refused() {
    assert_eq!(codes(&extern_add("library: l, abi: c")), vec!["KSEM180"]);
}

#[test]
fn a_duplicate_field_is_refused() {
    assert_eq!(
        codes(&extern_add("library: l, library: m, symbol: s, abi: c")),
        vec!["KSEM179"]
    );
}

#[test]
fn an_unknown_field_is_refused() {
    assert_eq!(
        codes(&extern_add("library: l, symbol: s, abi: c, bogus: x")),
        vec!["KSEM178"]
    );
}

#[test]
fn a_non_c_abi_is_refused() {
    assert_eq!(
        codes(&extern_add("library: l, symbol: s, abi: rust")),
        vec!["KSEM181"]
    );
}

// ----- annotation conflicts -----------------------------------------------

#[test]
fn ffi_extern_conflicts_with_each_execution_and_entry_annotation() {
    for marker in ["@Main", "@Runtime", "@Native", "@Export"] {
        assert_eq!(
            codes(&extern_with_marker(marker)),
            vec!["KSEM177"],
            "`@FFI.Extern` with {marker} must be one KSEM177 conflict"
        );
    }
}

// ----- name collisions ----------------------------------------------------

#[test]
fn a_foreign_name_may_not_collide_with_a_user_function() {
    let text = "function dup() -> I32 { return 1 }\n\
                @FFI.Extern { library: l, symbol: s, abi: c } function dup() -> I32\n\
                @Main function main() { print(dup()) return }";
    assert!(
        codes(text).iter().any(|code| code == "KSEM184"),
        "{:?}",
        codes(text)
    );
}

#[test]
fn two_foreign_functions_may_not_share_a_name() {
    let text = "@FFI.Extern { library: l, symbol: s, abi: c } function dup() -> I32\n\
                @FFI.Extern { library: l, symbol: t, abi: c } function dup() -> I32\n\
                @Main function main() { return }";
    assert!(
        codes(text).iter().any(|code| code == "KSEM185"),
        "{:?}",
        codes(text)
    );
}

// ----- one name, several shapes of one symbol -----------------------------

#[test]
fn one_name_carries_several_shapes_of_one_symbol() {
    let text = "@FFI.Extern { library: objc, symbol: objc_msgSend, abi: c }\n\
                function send(a: RawPtr, b: RawPtr) -> RawPtr\n\
                @FFI.Extern { library: objc, symbol: objc_msgSend, abi: c }\n\
                function send(a: RawPtr, b: RawPtr, c: U64) -> RawPtr\n\
                @Main function main() { return }";
    assert_eq!(codes(text), Vec::<String>::new());
}

#[test]
fn a_repeated_name_binding_a_different_symbol_is_still_refused() {
    let text = "@FFI.Extern { library: objc, symbol: objc_msgSend, abi: c }\n\
                function send(a: RawPtr) -> RawPtr\n\
                @FFI.Extern { library: objc, symbol: objc_msgSendSuper, abi: c }\n\
                function send(a: RawPtr, b: RawPtr) -> RawPtr\n\
                @Main function main() { return }";
    assert!(
        codes(text).iter().any(|code| code == "KSEM185"),
        "{:?}",
        codes(text)
    );
}

#[test]
fn a_repeated_shape_of_one_symbol_is_refused() {
    let text = "@FFI.Extern { library: objc, symbol: objc_msgSend, abi: c }\n\
                function send(a: RawPtr) -> RawPtr\n\
                @FFI.Extern { library: objc, symbol: objc_msgSend, abi: c }\n\
                function send(b: RawPtr) -> RawPtr\n\
                @Main function main() { return }";
    assert!(
        codes(text).iter().any(|code| code == "KSEM185"),
        "{:?}",
        codes(text)
    );
}

#[test]
fn the_number_of_arguments_picks_the_shape() {
    let text = "@FFI.Extern { library: objc, symbol: objc_msgSend, abi: c }\n\
                function send(a: RawPtr) -> RawPtr\n\
                @FFI.Extern { library: objc, symbol: objc_msgSend, abi: c }\n\
                function send(a: RawPtr, b: U64) -> RawPtr\n\
                @Main function main() {\n\
                    let one: RawPtr = send(RawPtr.null)\n\
                    let two: RawPtr = send(RawPtr.null, U64(1))\n\
                    return\n\
                }";
    assert_eq!(codes(text), Vec::<String>::new());
}

#[test]
fn the_argument_types_pick_the_shape() {
    let text = "@FFI.Extern { library: objc, symbol: objc_msgSend, abi: c }\n\
                function send(a: RawPtr, b: U64) -> RawPtr\n\
                @FFI.Extern { library: objc, symbol: objc_msgSend, abi: c }\n\
                function send(a: RawPtr, b: Bool) -> RawPtr\n\
                @Main function main() {\n\
                    let one: RawPtr = send(RawPtr.null, U64(1))\n\
                    let two: RawPtr = send(RawPtr.null, true)\n\
                    return\n\
                }";
    assert_eq!(codes(text), Vec::<String>::new());
}

#[test]
fn what_the_caller_expects_picks_between_shapes_differing_only_in_the_result() {
    let text = "@FFI.Extern { library: objc, symbol: objc_msgSend, abi: c }\n\
                function send(a: RawPtr) -> RawPtr\n\
                @FFI.Extern { library: objc, symbol: objc_msgSend, abi: c }\n\
                function send(a: RawPtr) -> U64\n\
                @Main function main() {\n\
                    let word: U64 = send(RawPtr.null)\n\
                    let pointer: RawPtr = send(RawPtr.null)\n\
                    return\n\
                }";
    assert_eq!(codes(text), Vec::<String>::new());
}

#[test]
fn a_call_on_a_line_by_itself_means_the_shape_that_answers_nothing() {
    let text = "@FFI.Extern { library: objc, symbol: objc_msgSend, abi: c }\n\
                function send(a: RawPtr) -> RawPtr\n\
                @FFI.Extern { library: objc, symbol: objc_msgSend, abi: c }\n\
                function send(a: RawPtr): Void\n\
                @Main function main() {\n\
                    send(RawPtr.null)\n\
                    return\n\
                }";
    assert_eq!(codes(text), Vec::<String>::new());
}

#[test]
fn a_condition_picks_the_shape_that_answers_a_bool() {
    let text = "@FFI.Extern { library: objc, symbol: objc_msgSend, abi: c }\n\
                function send(a: RawPtr) -> RawPtr\n\
                @FFI.Extern { library: objc, symbol: objc_msgSend, abi: c }\n\
                function send(a: RawPtr) -> Bool\n\
                @Main function main() {\n\
                    if send(RawPtr.null) { return }\n\
                    return\n\
                }";
    assert_eq!(codes(text), Vec::<String>::new());
}

#[test]
fn shapes_differing_only_in_the_result_are_refused_where_nothing_expects_one() {
    let text = "@FFI.Extern { library: objc, symbol: objc_msgSend, abi: c }\n\
                function send(a: RawPtr) -> RawPtr\n\
                @FFI.Extern { library: objc, symbol: objc_msgSend, abi: c }\n\
                function send(a: RawPtr) -> U64\n\
                @Main function main() {\n\
                    let it = send(RawPtr.null)\n\
                    return\n\
                }";
    assert!(
        codes(text).iter().any(|code| code == "KSEM372"),
        "{:?}",
        codes(text)
    );
}

#[test]
fn an_argument_count_no_shape_takes_names_the_counts_that_exist() {
    let text = "@FFI.Extern { library: objc, symbol: objc_msgSend, abi: c }\n\
                function send(a: RawPtr) -> RawPtr\n\
                @FFI.Extern { library: objc, symbol: objc_msgSend, abi: c }\n\
                function send(a: RawPtr, b: U64) -> RawPtr\n\
                @Main function main() {\n\
                    let it: RawPtr = send(RawPtr.null, U64(1), U64(2))\n\
                    return\n\
                }";
    assert!(
        codes(text).iter().any(|code| code == "KSEM062"),
        "{:?}",
        codes(text)
    );
}

#[test]
fn retains_names_real_parameters_once() {
    let unknown = r#"
@FFI.Extern { library: fixture, symbol: keep, abi: c, retains: missing }
function keep(text: CString): Void
"#;
    assert_eq!(library_codes(unknown), vec!["KSEM285"]);

    let duplicate = r#"
@FFI.Extern {
    library: fixture,
    symbol: keep,
    abi: c,
    retains: text,
    retains: text
}
function keep(text: CString): Void
"#;
    assert_eq!(library_codes(duplicate), vec!["KSEM286"]);
}

#[test]
fn an_address_declaration_answers_a_pointer_and_nothing_else() {
    // The address of a data symbol is a pointer word. A narrower result would
    // be a claim about the bytes stored there, which is a different question
    // and one this form cannot answer.
    let narrow = r#"
@FFI.Address { library: fixture, symbol: gState }
function state() -> I32
"#;
    assert_eq!(library_codes(narrow), vec!["KSEM369"]);

    let text = r#"
@FFI.Address { library: fixture, symbol: gName }
function name() -> CString
"#;
    assert_eq!(library_codes(text), vec!["KSEM369"]);

    // A named pointer type is the same word wearing a name, so it is accepted
    // for what it is.
    let named = r#"
@FFI.Struct { layout: c }
struct State { var tag: I32 }

@FFI.Pointer { target: State, ownership: borrowed }
struct StatePtr {}

@FFI.Address { library: fixture, symbol: gState }
function state() -> StatePtr
"#;
    assert!(library_diagnostics(named).is_empty(), "{named}");
}

#[test]
fn two_declarations_of_one_symbol_must_agree() {
    // Two Kira names for one C entry point are the same contract written
    // twice, which a binding file beside a hand-written declaration produces.
    let agreeing = r#"
@FFI.Extern { library: fixture, symbol: ffi_add, abi: c }
function add(a: I32, b: I32) -> I32

@FFI.Extern { library: fixture, symbol: ffi_add, abi: c }
function plus(a: I32, b: I32) -> I32
"#;
    assert!(
        library_diagnostics(agreeing).is_empty(),
        "{:?}",
        library_diagnostics(agreeing)
    );

    // Disagreeing is not: the linker binds one address, and each call site
    // marshals for whichever declaration it resolved.
    let disagreeing = r#"
@FFI.Extern { library: fixture, symbol: ffi_add, abi: c }
function add(a: I32, b: I32) -> I32

@FFI.Extern { library: fixture, symbol: ffi_add, abi: c }
function addWide(a: Int, b: Int) -> Int
"#;
    assert_eq!(library_codes(disagreeing), vec!["KSEM370"]);

    // A `retains:` on one and not the other is a disagreement about ownership,
    // which is the half a signature comparison could quietly drop.
    let lifetimes = r#"
@FFI.Extern { library: fixture, symbol: ffi_keep, abi: c }
function keep(text: CString): Void

@FFI.Extern { library: fixture, symbol: ffi_keep, abi: c, retains: text }
function keepOwned(text: CString): Void
"#;
    assert_eq!(library_codes(lifetimes), vec!["KSEM370"]);

    // The same symbol in two different libraries is two symbols.
    let separate = r#"
@FFI.Extern { library: alpha, symbol: ffi_add, abi: c }
function add(a: I32, b: I32) -> I32

@FFI.Extern { library: beta, symbol: ffi_add, abi: c }
function addWide(a: Int, b: Int) -> Int
"#;
    assert!(
        library_diagnostics(separate).is_empty(),
        "{:?}",
        library_diagnostics(separate)
    );
}

#[test]
fn retains_names_a_parameter_that_c_can_keep_by_ownership() {
    // A number has no block to transfer, so `retains:` on one promises a
    // transfer that cannot happen and makes the call site write `move` for it.
    let scalar = r#"
@FFI.Extern { library: fixture, symbol: ffi_keep, abi: c, retains: count }
function keep(count: I32): Void
"#;
    assert_eq!(library_codes(scalar), vec!["KSEM371"]);

    let flag = r#"
@FFI.Extern { library: fixture, symbol: ffi_keep, abi: c, retains: on }
function keep(on: Bool): Void
"#;
    assert_eq!(library_codes(flag), vec!["KSEM371"]);

    // C storage and callback-state ownership are both meaningful retain targets.
    let carriers = r#"
@FFI.Struct { layout: c }
struct Desc { var label: CString }

@FFI.Pointer { target: Desc, ownership: borrowed }
struct DescPtr {}

struct State { var count: Int }

@FFI.Extern { library: fixture, symbol: ffi_keep_text, abi: c, retains: text }
function keepText(text: CString): Void

@FFI.Extern { library: fixture, symbol: ffi_keep_desc, abi: c, retains: desc }
function keepDesc(desc: Desc): Void

@FFI.Extern { library: fixture, symbol: ffi_keep_ptr, abi: c, retains: desc }
function keepPtr(desc: DescPtr): Void

@FFI.Extern { library: fixture, symbol: ffi_keep_state, abi: c, retains: state }
function keepState(state: NativeState<State>): Void
"#;
    assert!(
        library_diagnostics(carriers).is_empty(),
        "{:?}",
        library_diagnostics(carriers)
    );
}
