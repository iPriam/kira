//! Semantic-analysis tests for closures: function types, capture rules, and
//! what calling a closure value checks.

use super::{codes, diagnostics};

#[test]
fn a_function_type_checks_in_every_position() {
    assert!(
        codes(
            "function apply(f: borrow (Int) -> Int, x: Int) -> Int { return f(x) }\n\
             function make(step: Int): (Int) -> Int { return { v in return v + step } }\n\
             @Main function main() { let add = make(2) print(apply(add, 3)) return }"
        )
        .is_empty()
    );
}

#[test]
fn a_named_function_is_a_function_value() {
    assert!(
        codes(
            "function double(value: Int) -> Int { return value * 2 }\n\
             function apply(f: borrow (Int) -> Int, value: Int) -> Int { return f(value) }\n\
             @Main function main() {\n\
                 let inferred = double\n\
                 let explicit: (Int) -> Int = double\n\
                 print(apply(inferred, 20) + explicit(1))\n\
                 return\n\
             }"
        )
        .is_empty()
    );
}

#[test]
fn a_named_function_must_match_its_expected_type() {
    assert_eq!(
        codes(
            "function text() -> String { return \"no\" }\n\
             @Main function main() { let value: () -> Int = text print(value()) return }"
        ),
        vec!["KSEM212"]
    );
}

#[test]
fn a_closure_with_no_expected_type_is_refused() {
    // Nothing at a `print` argument says what the parameters are, so there is
    // no signature to check the body against — and guessing one is exactly what
    // this refuses to do.
    assert_eq!(
        codes("@Main function main() { print({ v in return v }) return }"),
        vec!["KSEM134"]
    );
}

#[test]
fn a_closure_whose_parameter_count_is_wrong_is_refused() {
    assert_eq!(
        codes(
            "function run(f: (Int) -> Int) -> Int { return f(1) }\n\
             @Main function main() { print(run { a, b in return a }) return }"
        ),
        vec!["KSEM135"]
    );
}

#[test]
fn capturing_a_var_is_refused() {
    // The oracle *borrows* a mutable capture, which needs shared storage.
    // Nothing in this runtime shares storage, so copying it would run and give
    // the wrong answer — it is refused instead of silently diverging.
    assert_eq!(
        codes(
            "function run(f: () -> Int) -> Int { return f() }\n\
             @Main function main() { var total = 0 print(run { in return total }) return }"
        ),
        vec!["KSEM117"]
    );
}

#[test]
fn assigning_to_a_captured_binding_is_refused() {
    let reported = codes(
        "function run(f: () -> Void) { f() return }\n\
         @Main function main() { var total = 0 run { in total = total + 1 } print(total) return }",
    );
    assert!(
        reported.contains(&"KSEM117"),
        "a closure that writes an enclosing `var` is refused, got {reported:?}"
    );
}

#[test]
fn capturing_a_non_trivially_copyable_value_is_refused() {
    // `isTriviallyCopyable` admits only the scalars: a `String` capture is the
    // "non-Copy owned capture" KSEM117 names.
    assert_eq!(
        codes(
            "function run(f: () -> String) -> String { return f() }\n\
             @Main function main() { let label = \"hi\" print(run { in return label }) return }"
        ),
        vec!["KSEM117"]
    );

    // An array is a heap object too, and refused by the same rule.
    assert_eq!(
        codes(
            "function run(f: () -> Int) -> Int { return f() }\n\
             @Main function main() { let xs: [Int] = [1] print(run { in return xs.count }) return }"
        ),
        vec!["KSEM117"]
    );
}

#[test]
fn a_closure_argument_is_type_checked() {
    assert_eq!(
        codes(
            "@Main function main() { let f: (Int) -> Int = { v in return v } print(f(\"no\")) return }"
        ),
        vec!["KSEM063"]
    );
}

#[test]
fn a_closure_call_checks_its_argument_count() {
    assert_eq!(
        codes(
            "@Main function main() { let f: (Int) -> Int = { v in return v } print(f(1, 2)) return }"
        ),
        vec!["KSEM062"]
    );
}

#[test]
fn a_closure_body_is_checked_against_its_result_type() {
    assert_eq!(
        codes(
            "function run(f: () -> Int) -> Int { return f() }\n\
             @Main function main() { print(run { in return \"no\" }) return }"
        ),
        vec!["KSEM032"]
    );
}

#[test]
fn two_spellings_of_one_function_type_are_one_type() {
    // `(Int) -> Int` written in a parameter, an annotation, and a return type
    // interns to a single type, so a closure made for one fits all three.
    assert!(
        codes(
            "function apply(f: borrow (Int) -> Int) -> Int { return f(1) }\n\
             function make(): (Int) -> Int { return { v in return v } }\n\
             @Main function main() {\n\
               let a: (Int) -> Int = { v in return v }\n\
               let b = make()\n\
               print(apply(a) + apply(b))\n\
               return\n\
             }"
        )
        .is_empty()
    );
}

#[test]
fn a_closure_has_no_receiver_to_read_a_bare_field_from() {
    // A closure lifted out of a method has no `self`, so a bare field name in
    // its body resolves to nothing — which is what it is, not a capture.
    assert_eq!(
        codes(
            "class Counter {\n\
               let step: Int = 1\n\
               function run(f: () -> Int) -> Int { return f() }\n\
               function go() -> Int { return self.run({ in return step }) }\n\
             }\n\
             @Main function main() { print(Counter().go()) return }"
        ),
        vec!["KSEM060"]
    );
}

#[test]
fn a_closure_may_be_declared_but_never_called() {
    // A function type with no call site mints no dispatcher; a value of it is
    // still built and still copied and dropped like any other struct.
    assert!(
        codes("@Main function main() { let f: (Int) -> Int = { v in return v } return }")
            .is_empty()
    );
}

#[test]
fn capturing_a_moved_local_is_rejected() {
    // A capture is a read of the enclosing binding, so it answers to the move
    // checker. Without the check in `capture` the closure body only ever sees
    // the fresh inner binding, which was never moved out of, and the stale
    // value would be read.
    assert_eq!(
        codes(
            "struct Mesh { let id: Int }\n\
             function consume(mesh: Mesh) -> Int { return mesh.id }\n\
             @Main function main() { var mesh = Mesh { id: 3 } \
             print(consume(move mesh)) \
             let f: () -> Int = { in return mesh.id } print(f()) return }"
        ),
        vec!["KSEM107"]
    );
}

#[test]
fn a_function_value_has_no_members() {
    // The representation struct is an implementation detail of the desugar.
    // `tag` is a legal identifier and the repr is an ordinary struct, so
    // without a refusal here `f.tag` would resolve and print 0 — surface the
    // oracle does not have.
    assert_eq!(
        codes(
            "@Main function main() { let f: (Int) -> Int = { v in return v } \
             print(f.tag) return }"
        ),
        vec!["KSEM136"]
    );
}

// ----- ownership modes on a function type ---------------------------------

/// A `borrow` parameter on a function type is checked at the indirect call:
/// the callee only reads, so the argument needs no `move`.
#[test]
fn a_borrow_parameter_on_a_function_type_takes_no_move() {
    let text = "struct Event { let code: Int }\n\
                    function handle(event: borrow Event) { print(event.code) return }\n\
                    @Main function main() { \
                    let onEvent: (borrow Event) -> Void = handle \
                    let e = Event { code: 1 } onEvent(e) return }";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
}

/// Without the mode the parameter is owned, and the same call does need `move`
/// — which is the whole reason the mode is carried rather than dropped.
#[test]
fn an_owned_parameter_on_a_function_type_still_demands_move() {
    let text = "struct Event { let code: Int }\n\
                    function handle(event: Event) { print(event.code) return }\n\
                    @Main function main() { \
                    let onEvent: (Event) -> Void = handle \
                    let e = Event { code: 1 } onEvent(e) return }";
    assert_eq!(codes(text), vec!["KSEM108"]);
}

/// The mode is part of the type, so a function declaring one mode does not fit
/// a slot declaring another.
#[test]
fn a_function_type_does_not_match_one_differing_only_in_a_mode() {
    let text = "struct Event { let code: Int }\n\
                    function handle(event: Event) { print(event.code) return }\n\
                    @Main function main() { \
                    let onEvent: (borrow Event) -> Void = handle return }";
    assert_eq!(codes(text), vec!["KSEM212"]);
}

/// A `borrow mut` parameter may be written on a function type, assigned a
/// matching function, and *called through* — the writeback rides the dispatcher.
#[test]
fn a_borrow_mut_function_type_declares_and_calls() {
    let text = "struct Frame { var n: Int }\n\
                    function bump(frame: borrow mut Frame) { frame.n = frame.n + 1 return }\n\
                    @Main function main() { \
                    let onFrame: (borrow mut Frame) -> Void = bump \
                    var f = Frame { n: 1 } onFrame(f) print(f.n) return }";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
}

/// A closure literal's parameter takes the mode its type declares, so a body
/// that writes through a `borrow mut` parameter is not refused for mutating an
/// owned binding.
#[test]
fn a_closure_may_write_through_a_borrow_mut_parameter() {
    let text = "struct Frame { var n: Int }\n\
                    @Main function main() { \
                    let onFrame: (borrow mut Frame) -> Void = { f in f.n = f.n + 1 return } \
                    var f = Frame { n: 1 } onFrame(f) print(f.n) return }";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
}

/// The temporary rule holds through a function value too: there is nowhere in
/// the caller for the callee's write to land.
#[test]
fn a_temporary_is_refused_as_a_borrow_mut_argument_through_a_function_value() {
    let text = "struct Frame { var n: Int }\n\
                    function bump(frame: borrow mut Frame) { frame.n = frame.n + 1 return }\n\
                    @Main function main() { \
                    let onFrame: (borrow mut Frame) -> Void = bump \
                    onFrame(Frame { n: 1 }) return }";
    assert_eq!(codes(text), vec!["KSEM248"]);
}

// ----- an ownership prefix on a binding's annotation -----------------------

/// A binding may carry the same ownership prefix a parameter may, and a
/// `borrow` one is honored for a type an owned binding leaves alone anyway.
#[test]
fn a_borrow_prefix_on_a_binding_annotation_is_accepted() {
    let text = "struct Frame { var n: Int }\n\
                    function bump(frame: borrow mut Frame) { frame.n = frame.n + 1 return }\n\
                    @Main function main() { \
                    let onFrame: borrow (borrow mut Frame) -> Void = bump \
                    var f = Frame { n: 1 } onFrame(f) print(f.n) return }";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
}

/// The prefix is part of the *binding*, not the type, so it does not change
/// which function type the annotation names.
#[test]
fn a_borrow_prefix_does_not_change_the_annotated_type() {
    let text = "function double(v: Int) -> Int { return v * 2 }\n\
                    function apply(f: borrow (Int) -> Int, v: Int) -> Int { return f(v) }\n\
                    @Main function main() { \
                    let a: borrow (Int) -> Int = double \
                    let b: (Int) -> Int = double \
                    print(apply(a, 1) + apply(b, 2)) return }";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
}

/// A type that aliases its source cannot be borrowed by a binding: nothing
/// shares storage, so the binding would read as a view over a snapshot.
#[test]
fn a_borrow_prefix_on_an_aliasing_type_is_refused() {
    assert_eq!(
        codes(
            "@Main function main() { let xs: [Int] = [1] let ys: borrow [Int] = xs print(ys.count) return }"
        ),
        vec!["KSEM250"]
    );
}

/// `borrow mut` on a binding is refused whatever the type: a write through it
/// has no caller to land in.
#[test]
fn a_borrow_mut_prefix_on_a_binding_is_refused() {
    assert_eq!(
        codes(
            "struct Frame { var n: Int }\n\
             @Main function main() { var f = Frame { n: 1 } \
             let g: borrow mut Frame = f print(g.n) return }"
        ),
        vec!["KSEM250"]
    );
}

/// `move` on a binding consumes its initializer, whatever the type — which is
/// what separates it from the implicit move, that only fires for an aliasing
/// one.
#[test]
fn a_move_prefix_on_a_binding_consumes_its_initializer() {
    assert_eq!(
        codes(
            "struct Frame { var n: Int }\n\
             @Main function main() { let f = Frame { n: 1 } \
             let g: move Frame = f print(f.n) return }"
        ),
        vec!["KSEM107"]
    );
}

/// A binding *named* `borrow` still parses as one: the prefix is contextual, so
/// it is committed to only when a type follows.
#[test]
fn a_binding_named_borrow_still_parses() {
    assert!(
        diagnostics("@Main function main() { let borrow: Int = 1 print(borrow) return }")
            .is_empty()
    );
}

// ----- what a closure may carry ------------------------------------------

/// A `RawPtr` capture copies a word and frees nothing, so it needs no `copy`.
#[test]
fn a_raw_pointer_may_be_captured() {
    let text = "struct Host { var seed: Int }\n\
                @Main function main() { \
                let boxed = nativeState(Host { seed: 1 }) \
                let handle = nativeUserData(boxed) \
                let f: () -> Int = { in var h = nativeRecover<Host>(handle) return h.seed } \
                print(f()) nativeStateFree(boxed) return }";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
}

/// A function value of *another* function type nests: one representation struct
/// holding another is an ordinary struct.
#[test]
fn a_function_value_of_another_type_may_be_captured() {
    let text = "@Main function main() { \
                let step: (Int) -> Int = { v in return v + 1 } \
                let run: () -> Int = { in return step(1) } \
                print(run()) return }";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
}

/// A capture of the closure's *own* function type has no representation: the
/// value would sit inside a value of its own type.
#[test]
fn a_function_value_of_the_closures_own_type_is_refused() {
    let text = "@Main function main() { \
                let step: (Int) -> Int = { v in return v + 1 } \
                let again: (Int) -> Int = { v in return step(v) } \
                print(again(1)) return }";
    assert_eq!(codes(text), vec!["KSEM251"]);
}

/// Callback state may hold a function value: it is a tag plus captures that
/// each had to be trivially copyable, so it boxes like any other struct.
#[test]
fn callback_state_may_hold_a_function_value() {
    let text = "struct Frame { var n: Int }\n\
                function bump(frame: borrow mut Frame) { frame.n = frame.n + 1 return }\n\
                struct AppState { var count: Int\n var onFrame: (borrow mut Frame) -> Void }\n\
                @Main function main() { \
                let boxed = nativeState(AppState { count: 1, onFrame: bump }) \
                var back = nativeRecover<AppState>(nativeUserData(boxed)) \
                var f = Frame { n: 1 } back.onFrame(f) print(f.n) \
                nativeStateFree(boxed) return }";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
}
