//! Semantic-analysis tests for generic enum declarations.
//!
//! A generic enum declares no type: each written instantiation monomorphizes
//! into an ordinary enum. So these tests pin two things — that a correct
//! instantiation type-checks like the hand-written enum it becomes, and that
//! every way of getting it wrong is a typed diagnostic rather than a panic.

use super::codes;

/// The oracle's `Result` verbatim, plus the failure enum a use site needs.
const RESULT: &str = "enum AppError { NotFound Denied }\n\
                      enum Result<Value, Failure> {\n\
                          Ok(Value)\n\
                          Error(Failure)\n\
                      }\n";

#[test]
fn the_oracles_result_declares_and_instantiates() {
    assert!(
        codes(&format!(
            "{RESULT}\
             function find(flag: Bool) -> Result<Int, AppError> {{\n\
                 if flag {{ return .Ok(12) }}\n\
                 return .Error(.NotFound)\n\
             }}\n\
             @Main function main() {{\n\
                 let found: Result<Int, AppError> = find(true)\n\
                 match found {{ Ok -> {{ print(1) }} Error -> {{ print(0) }} }}\n\
                 return\n\
             }}"
        ))
        .is_empty()
    );
}

#[test]
fn leading_dot_construction_works_against_a_generic_instantiation() {
    // `.Ok(12)` has to resolve against the *monomorphized* enum, and its
    // payload has to type-check as `Int` — the argument, not the parameter.
    assert!(
        codes(&format!(
            "{RESULT}\
             @Main function main() {{\n\
                 let ok: Result<Int, AppError> = .Ok(12)\n\
                 let bad: Result<Int, AppError> = .Error(.Denied)\n\
                 if ok == bad {{ print(1) }}\n\
                 return\n\
             }}"
        ))
        .is_empty()
    );
}

#[test]
fn a_payload_that_disagrees_with_the_type_argument_is_reported() {
    // `Ok(Value)` with `Value = Int` may not take a `String`. The mistake is a
    // payload type error, not a generics error — which is the point.
    assert!(
        !codes(&format!(
            "{RESULT}\
         @Main function main() {{\n\
             let ok: Result<Int, AppError> = .Ok(\"twelve\")\n\
             print(1)\n\
             return\n\
         }}"
        ))
        .is_empty()
    );
}

#[test]
fn two_writings_of_one_instantiation_are_the_same_type() {
    // The mangled name is the memo key, so both annotations must find the same
    // row — otherwise this assignment would be a type mismatch.
    assert!(
        codes(&format!(
            "{RESULT}\
             function first() -> Result<Int, AppError> {{ return .Ok(1) }}\n\
             @Main function main() {{\n\
                 let same: Result<Int, AppError> = first()\n\
                 match same {{ Ok -> {{ print(1) }} Error -> {{ print(0) }} }}\n\
                 return\n\
             }}"
        ))
        .is_empty()
    );
}

#[test]
fn two_different_instantiations_are_different_types() {
    assert!(
        !codes(&format!(
            "{RESULT}\
         function first() -> Result<Int, AppError> {{ return .Ok(1) }}\n\
         @Main function main() {{\n\
             let other: Result<String, AppError> = first()\n\
             print(1)\n\
             return\n\
         }}"
        ))
        .is_empty()
    );
}

#[test]
fn an_arity_mismatch_is_reported() {
    assert!(
        codes(&format!(
            "{RESULT}\
             @Main function main() {{ let x: Result<Int> = .Ok(1) print(1) return }}"
        ))
        .contains(&"KSEM174")
    );
    assert!(
        codes(&format!(
            "{RESULT}\
             @Main function main() {{\n\
                 let x: Result<Int, AppError, Bool> = .Ok(1)\n\
                 print(1)\n\
                 return\n\
             }}"
        ))
        .contains(&"KSEM174")
    );
}

#[test]
fn a_generic_enum_written_bare_says_what_is_missing() {
    assert!(
        codes(&format!(
            "{RESULT}\
             @Main function main() {{ let x: Result = .Ok(1) print(1) return }}"
        ))
        .contains(&"KSEM172")
    );
}

#[test]
fn type_arguments_on_a_non_generic_type_are_refused() {
    assert!(
        codes(
            "enum Color { Red Green }\n\
             @Main function main() {{ let x: Color<Int> = .Red print(1) return }}"
        )
        .contains(&"KSEM173")
    );
    assert!(
        codes("@Main function main() { let x: Int<Bool> = 1 print(x) return }")
            .contains(&"KSEM173")
    );
}

#[test]
fn an_unknown_generic_name_is_an_unknown_type() {
    assert!(
        codes("@Main function main() { let x: Missing<Int> = 1 print(1) return }")
            .contains(&"KSEM050")
    );
}

#[test]
fn a_type_parameter_may_not_shadow_a_builtin() {
    assert!(
        codes("enum Box<Int> { One(Int) }\n@Main function main() { print(1) return }")
            .contains(&"KSEM170")
    );
}

#[test]
fn a_repeated_type_parameter_is_reported() {
    assert!(
        codes("enum Pair<Value, Value> { One(Value) }\n@Main function main() { print(1) return }")
            .contains(&"KSEM171")
    );
}

#[test]
fn a_duplicate_generic_enum_is_reported() {
    assert!(
        codes(
            "enum Result<A, B> { Ok(A) Error(B) }\n\
             enum Result<A, B> { Ok(A) Error(B) }\n\
             @Main function main() { print(1) return }"
        )
        .contains(&"KSEM169")
    );
}

#[test]
fn a_template_that_grows_its_own_argument_is_refused_not_overflowed() {
    // `Grow<[Value]>` mints a fresh mangled name every round, so the memo cannot
    // stop it — the depth cap does, and it must be a diagnostic.
    assert!(
        codes(
            "enum Grow<Value> { More(Grow<[Value]>) }\n\
             @Main function main() { let x: Grow<Int> = .More(.More(.More(.More(x)))) return }"
        )
        .contains(&"KSEM175")
    );
}

#[test]
fn an_unbound_type_name_in_a_template_body_is_an_unknown_type() {
    // `Missing` is not a parameter of `Holder` and not a declared type, so the
    // template body's own mistake surfaces at the instantiation.
    assert!(
        !codes(
            "enum Holder<Value> { One(Value) Two(Missing) }\n\
         @Main function main() { let x: Holder<Int> = .One(1) print(1) return }"
        )
        .is_empty()
    );
}

#[test]
fn a_type_parameter_does_not_leak_out_of_its_own_template() {
    // `Inner` never declares `Value`, so `Value` inside it is unknown even
    // though the enclosing instantiation binds one.
    assert!(
        !codes(
            "enum Inner<A> { Held(Value) }\n\
         enum Outer<Value> { Wrap(Inner<Value>) }\n\
         @Main function main() { let x: Outer<Int> = .Wrap(.Held(1)) print(1) return }"
        )
        .is_empty()
    );
}

#[test]
fn attempt_and_try_resolve_a_generic_result_nominally() {
    // The `Result`-shaped check is structural, and a monomorphized
    // `Result<Int, AppError>` *is* an enum with `Ok(Int)` and `Error(AppError)`
    // — so the oracle's own `Result` flows through `try` with nothing added.
    assert!(
        codes(&format!(
            "{RESULT}\
             function find(n: Int) -> Result<Int, AppError> {{\n\
                 if n < 0 {{ return .Error(.NotFound) }}\n\
                 return .Ok(n)\n\
             }}\n\
             function doubled(n: Int) -> Int {{\n\
                 attempt {{\n\
                     let v = try find(n)\n\
                     return v * 2\n\
                 }} handle {{\n\
                     NotFound {{ return 0 - 1 }}\n\
                     Denied {{ return 0 - 2 }}\n\
                 }}\n\
             }}\n\
             @Main function main() {{ print(doubled(5)) print(doubled(0 - 1)) return }}"
        ))
        .is_empty()
    );
}

#[test]
fn a_qualified_spelling_constructs_the_instantiation_the_position_asks_for() {
    // `Result.Ok(12)` carries no type arguments, so the annotation is what says
    // which instantiation it builds — exactly as `.Ok(12)` does. The template
    // name in front only has to agree with it.
    assert!(
        codes(&format!(
            "{RESULT}\
             function find() -> Result<Int, AppError> {{ return Result.Ok(1) }}\n\
             @Main function main() {{\n\
                 let ok: Result<Int, AppError> = Result.Ok(12)\n\
                 let bad: Result<Int, AppError> = Result.Error(.Denied)\n\
                 let from: Result<Int, AppError> = find()\n\
                 match ok {{ Ok -> {{ print(1) }} Error -> {{ print(0) }} }}\n\
                 match bad {{ Ok -> {{ print(1) }} Error -> {{ print(0) }} }}\n\
                 match from {{ Ok -> {{ print(1) }} Error -> {{ print(0) }} }}\n\
                 return\n\
             }}"
        ))
        .is_empty()
    );
}

#[test]
fn a_payload_less_variant_takes_a_qualified_spelling_too() {
    // No arguments means it parses as a field read rather than a call, which is
    // the other of the two paths a qualified spelling arrives on.
    assert!(
        codes(
            "enum Flag<Value> { On Off Held(Value) }\n\
             @Main function main() { let f: Flag<Int> = Flag.On print(1) return }"
        )
        .is_empty()
    );
}

#[test]
fn a_qualified_payload_is_checked_against_the_argument_not_the_parameter() {
    // `Ok(Value)` with `Value = Int` may not take a `String` — the same rule the
    // leading-dot form obeys, reached through the qualified spelling.
    assert!(
        !codes(&format!(
            "{RESULT}\
         @Main function main() {{\n\
             let ok: Result<Int, AppError> = Result.Ok(\"twelve\")\n\
             print(1)\n\
             return\n\
         }}"
        ))
        .is_empty()
    );
}

#[test]
fn a_qualified_spelling_with_no_instantiation_to_build_says_so() {
    // No annotation at all: nothing supplies the type arguments a constructor
    // cannot spell, so this is a typed refusal rather than "undefined name".
    assert!(
        codes(&format!(
            "{RESULT}\
             @Main function main() {{ let r = Result.Ok(1) print(1) return }}"
        ))
        .contains(&"KSEM254")
    );
    // A payload-less variant reaches the same refusal down the field path.
    assert!(
        codes(
            "enum Flag<Value> { On Held(Value) }\n\
             @Main function main() { let f = Flag.On print(1) return }"
        )
        .contains(&"KSEM254")
    );
}

#[test]
fn a_qualified_spelling_against_an_unrelated_expected_type_is_refused() {
    // The expectation is an enum, but not an instantiation of this template, so
    // the name in front disagrees with the position and neither one wins.
    assert!(
        codes(&format!(
            "{RESULT}\
             enum Color {{ Red Green }}\n\
             @Main function main() {{ let c: Color = Result.Ok(1) print(1) return }}"
        ))
        .contains(&"KSEM254")
    );
    // A non-enum expectation is the same mistake.
    assert!(
        codes(&format!(
            "{RESULT}\
             @Main function main() {{ let n: Int = Result.Ok(1) print(1) return }}"
        ))
        .contains(&"KSEM254")
    );
}

#[test]
fn one_templates_name_does_not_construct_anothers_instantiation() {
    // Both are generic and both have an `Ok`, so only the recorded template
    // separates them: `Other.Ok` may not build a `Result<Int, AppError>`.
    assert!(
        codes(&format!(
            "{RESULT}\
             enum Other<Value> {{ Ok(Value) }}\n\
             @Main function main() {{\n\
                 let r: Result<Int, AppError> = Other.Ok(1)\n\
                 print(1)\n\
                 return\n\
             }}"
        ))
        .contains(&"KSEM254")
    );
}

#[test]
fn a_hand_written_enums_name_does_not_construct_an_instantiation() {
    // `Color` is not a template, so `Color.Ok` is an ordinary qualified
    // spelling against `Color` — and `Color` has no `Ok`.
    assert!(
        codes(&format!(
            "{RESULT}\
             enum Color {{ Red Green }}\n\
             @Main function main() {{\n\
                 let r: Result<Int, AppError> = Color.Ok(1)\n\
                 print(1)\n\
                 return\n\
             }}"
        ))
        .contains(&"KSEM120")
    );
}

#[test]
fn a_variant_no_instantiation_has_is_still_a_missing_variant() {
    // The template is anchored, so the mistake is the variant name — which is a
    // sharper thing to say than "this needs an instantiation".
    assert!(
        codes(&format!(
            "{RESULT}\
             @Main function main() {{\n\
                 let r: Result<Int, AppError> = Result.Nope(1)\n\
                 print(1)\n\
                 return\n\
             }}"
        ))
        .contains(&"KSEM120")
    );
}

#[test]
fn a_local_named_like_a_template_wins_over_it() {
    // Every other qualifier here yields to a local of the same name, and a
    // template is no different — `Result.x` reads the struct's field.
    assert!(
        codes(&format!(
            "{RESULT}\
             struct Holder {{ var x: Int = 0 }}\n\
             @Main function main() {{\n\
                 let Result = Holder {{ x = 3 }}\n\
                 print(Result.x)\n\
                 return\n\
             }}"
        ))
        .is_empty()
    );
}

#[test]
fn a_generic_struct_class_and_function_are_refused_by_name() {
    assert!(
        codes("struct Box<Value> { let v: Int }\n@Main function main() { print(1) return }")
            .contains(&"KPAR047")
    );
    assert!(
        codes("class Box<Value> { let v: Int = 1 }\n@Main function main() { print(1) return }")
            .contains(&"KPAR047")
    );
    assert!(
        codes(
            "function id<Value>(v: Int) -> Int { return v }\n@Main function main() { print(1) return }"
        )
        .contains(&"KPAR047")
    );
}

#[test]
fn an_empty_type_parameter_list_is_reported() {
    assert!(
        codes("enum Result<> { Ok Error }\n@Main function main() { print(1) return }")
            .contains(&"KPAR046")
    );
}

#[test]
fn an_unclosed_type_parameter_list_is_reported_not_hung() {
    assert!(!codes("enum Result<Value { Ok(Value) }").is_empty());
    assert!(!codes("@Main function main() { let x: Result<Int = 1 return }").is_empty());
}

#[test]
fn a_nested_instantiation_closes_on_a_shifted_right_angle() {
    // `Result<Result<Int, AppError>, AppError>` closes on a single `>>` token,
    // which the parser splits in place.
    assert!(
        codes(&format!(
            "{RESULT}\
             @Main function main() {{\n\
                 let nested: Result<Result<Int, AppError>, AppError> = .Ok(.Ok(1))\n\
                 match nested {{ Ok -> {{ print(1) }} Error -> {{ print(0) }} }}\n\
                 return\n\
             }}"
        ))
        .is_empty()
    );
}
