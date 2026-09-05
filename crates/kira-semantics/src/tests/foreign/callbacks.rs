//! `@FFI.Callback`: a Kira function named where C expects a function pointer.

use super::*;

#[test]
fn a_kira_function_named_where_a_callback_is_expected_records_one_entry() {
    let text = "@FFI.Callback { abi: c, params: [I32, I32], result: I32 }\n\
                struct Adder {}\n\
                function combine(a: I32, b: I32) -> I32 { return a + b }\n\
                @FFI.Extern { library: l, symbol: s, abi: c }\n\
                function callAdder(add: Adder, a: I32, b: I32) -> I32\n\
                @Main function main() { print(callAdder(combine, 1, 2)) return }";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
    let program = program(text);
    assert_eq!(program.foreign_callbacks.len(), 1);
    let entry = &program.foreign_callbacks[0];
    assert_eq!(
        entry.signature().parameters(),
        &[
            ForeignTypeSpec::Scalar(ForeignType::I32),
            ForeignTypeSpec::Scalar(ForeignType::I32)
        ]
    );
}

#[test]
fn naming_the_same_function_twice_records_one_callback_entry() {
    let text = "@FFI.Callback { abi: c, params: [I32], result: Void }\n\
                struct Sink {}\n\
                function take(x: I32) -> Void { return }\n\
                @FFI.Extern { library: l, symbol: a, abi: c }\n\
                function first(s: Sink) -> Void\n\
                @FFI.Extern { library: l, symbol: b, abi: c }\n\
                function second(s: Sink) -> Void\n\
                @Main function main() { first(take)\n second(take) return }";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
    assert_eq!(program(text).foreign_callbacks.len(), 1);
}

#[test]
fn a_function_whose_signature_does_not_fit_the_callback_is_refused() {
    let wrong_result = "@FFI.Callback { abi: c, params: [I32], result: I32 }\n\
                        struct Adder {}\n\
                        function takes(x: I32) -> Void { return }\n\
                        @FFI.Extern { library: l, symbol: s, abi: c }\n\
                        function use_it(a: Adder) -> Void\n\
                        @Main function main() { use_it(takes) return }";
    assert_eq!(codes(wrong_result), vec!["KSEM246"]);

    let wrong_arity = "@FFI.Callback { abi: c, params: [I32], result: Void }\n\
                       struct Sink {}\n\
                       function takes(x: I32, y: I32) -> Void { return }\n\
                       @FFI.Extern { library: l, symbol: s, abi: c }\n\
                       function use_it(a: Sink) -> Void\n\
                       @Main function main() { use_it(takes) return }";
    assert_eq!(codes(wrong_arity), vec!["KSEM246"]);

    // A bare `Int` has no C width, so it is not a callback parameter either.
    let bare_int = "@FFI.Callback { abi: c, params: [I32], result: Void }\n\
                    struct Sink {}\n\
                    function takes(x: Int) -> Void { return }\n\
                    @FFI.Extern { library: l, symbol: s, abi: c }\n\
                    function use_it(a: Sink) -> Void\n\
                    @Main function main() { use_it(takes) return }";
    assert_eq!(codes(bare_int), vec!["KSEM246"]);
}

#[test]
fn a_callback_declaring_a_type_the_seam_cannot_carry_is_refused_where_it_is_filled() {
    // Declaring it is clean: a generated binding declares every callback its
    // headers name, and most are never filled.
    let declared = "@FFI.Callback { abi: c, params: [[I32]], result: Void }\n\
                    struct Sink {}\n\
                    @Main function main() { return }";
    assert!(
        diagnostics(declared).is_empty(),
        "{:?}",
        diagnostics(declared)
    );

    // Handing a Kira function to one is where it cannot work, and is reported.
    let filled = "@FFI.Callback { abi: c, params: [[I32]], result: Void }\n\
                  struct Sink {}\n\
                  function takes(x: [I32]) -> Void { return }\n\
                  @FFI.Extern { library: l, symbol: s, abi: c }\n\
                  function use_it(a: Sink) -> Void\n\
                  @Main function main() { use_it(takes) return }";
    assert_eq!(codes(filled), vec!["KSEM245"]);
}

/// A callback parameter C passes by value is recorded as the aggregate it is,
/// and the Kira function receives a pointer to it.
///
/// `WGPURequestAdapterCallback` is why: its `WGPUStringView` parameter is fixed
/// by Dawn's header, and `wgpuInstanceRequestAdapter` is the only route to an
/// adapter — so a binding that could not fill this callback could not reach a
/// device at all.
#[test]
fn a_struct_callback_parameter_is_an_aggregate_the_function_takes_by_pointer() {
    let text = "@FFI.Struct { layout: c }\n\
                struct View { let length: U64 }\n\
                @FFI.Pointer { target: View, ownership: borrowed }\n\
                struct ViewPtr {}\n\
                @FFI.Callback { abi: c, params: [I32, View], result: Void }\n\
                struct Sink {}\n\
                function takes(tag: I32, view: ViewPtr) -> Void { return }\n\
                @FFI.Extern { library: l, symbol: s, abi: c }\n\
                function use_it(a: Sink) -> Void\n\
                @Main function main() { use_it(takes) return }";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
    let program = program(text);
    assert_eq!(program.foreign_callbacks.len(), 1);
    let declared = program.foreign_callbacks[0].signature().parameters();
    assert_eq!(declared[0], ForeignTypeSpec::Scalar(ForeignType::I32));
    assert!(
        declared[1].aggregate().is_some(),
        "the struct position stays an aggregate on the wire: {declared:?}"
    );
}

/// The struct itself is not what such a function receives, and saying so is the
/// diagnostic — a copy would be a second image of storage C already owns.
#[test]
fn a_struct_callback_parameter_taken_by_value_in_kira_is_refused() {
    let by_value = "@FFI.Struct { layout: c }\n\
                    struct View { let length: U64 }\n\
                    @FFI.Callback { abi: c, params: [View], result: Void }\n\
                    struct Sink {}\n\
                    function takes(view: View) -> Void { return }\n\
                    @FFI.Extern { library: l, symbol: s, abi: c }\n\
                    function use_it(a: Sink) -> Void\n\
                    @Main function main() { use_it(takes) return }";
    assert_eq!(codes(by_value), vec!["KSEM246"]);

    // And a pointer to a *different* C-layout struct is a mistake the seam can
    // see, rather than a pointer word it waves through.
    let wrong_target = "@FFI.Struct { layout: c }\n\
                        struct View { let length: U64 }\n\
                        @FFI.Struct { layout: c }\n\
                        struct Other { let n: I32 }\n\
                        @FFI.Pointer { target: Other, ownership: borrowed }\n\
                        struct OtherPtr {}\n\
                        @FFI.Callback { abi: c, params: [View], result: Void }\n\
                        struct Sink {}\n\
                        function takes(view: OtherPtr) -> Void { return }\n\
                        @FFI.Extern { library: l, symbol: s, abi: c }\n\
                        function use_it(a: Sink) -> Void\n\
                        @Main function main() { use_it(takes) return }";
    assert_eq!(codes(wrong_target), vec!["KSEM246"]);
}

/// A callback *returning* a struct stays refused, and stays refused at the fill
/// site rather than at the declaration.
///
/// Not the same question as a parameter. A parameter is storage C already owns,
/// and its address is the whole answer; a result would have to be C-layout bytes
/// built out of a Kira value, which nothing on this seam carries back.
#[test]
fn a_struct_callback_result_is_refused_where_it_is_filled() {
    let text = "@FFI.Struct { layout: c }\n\
                struct View { let length: U64 }\n\
                @FFI.Callback { abi: c, params: [], result: View }\n\
                struct Sink {}\n\
                function gives() -> View { return View {} }\n\
                @FFI.Extern { library: l, symbol: s, abi: c }\n\
                function use_it(a: Sink) -> Void\n\
                @Main function main() { use_it(gives) return }";
    assert_eq!(codes(text), vec!["KSEM245"]);
}

/// A `String` callback parameter is the one Kira type that *does* fit a C
/// position: it carries the `const char*` C hands over, copied by the thunk.
#[test]
fn a_string_callback_parameter_carries_a_c_string() {
    let text = "@FFI.Callback { abi: c, params: [CString], result: Void }\n\
                struct Sink {}\n\
                function takes(x: String) -> Void { print(x) return }\n\
                @FFI.Extern { library: l, symbol: s, abi: c }\n\
                function use_it(a: Sink) -> Void\n\
                @Main function main() { use_it(takes) return }";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
}

/// The other direction has no answer: C would be handed a pointer somebody has
/// to free, and a Kira `String` belongs to Kira.
#[test]
fn a_string_callback_result_is_refused() {
    let text = "@FFI.Callback { abi: c, params: [], result: CString }\n\
                struct Sink {}\n\
                function gives() -> String { return \"x\" }\n\
                @FFI.Extern { library: l, symbol: s, abi: c }\n\
                function use_it(a: Sink) -> Void\n\
                @Main function main() { use_it(gives) return }";
    assert_eq!(codes(text), vec!["KSEM245"]);
}

#[test]
fn a_local_wins_over_a_function_of_the_same_name_in_a_callback_slot() {
    // A callback the program got from C, held in a variable named like a
    // function, is read as the variable.
    let text = "@FFI.Callback { abi: c, params: [I32], result: Void }\n\
                struct Sink {}\n\
                function handler(x: I32) -> Void { return }\n\
                @FFI.Extern { library: l, symbol: s, abi: c }\n\
                function use_it(a: Sink) -> Void\n\
                @Main function main() { let handler = Sink {}\n use_it(handler) return }";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
    assert!(
        program(text).foreign_callbacks.is_empty(),
        "the local is the value, so no entry is recorded"
    );
}
