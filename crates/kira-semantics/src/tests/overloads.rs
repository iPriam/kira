//! Overloading: several declarations answering to one name, and the rule that
//! picks among them at a call.

use crate::tests::{codes, library_codes};

/// Declarations sharing a name and differing in what they take are separate
/// declarations, and every call of them resolves.
#[test]
fn functions_sharing_a_name_and_differing_in_parameters_are_separate() {
    assert!(
        codes(
            r#"
function describe(n: Int) -> Int { return 1 }
function describe(text: String) -> Int { return 2 }
function describe(a: Int, b: Int) -> Int { return 3 }
function describe(f: Float) -> Int { return 4 }

@Main
function main() {
    print(describe(1))
    print(describe("a"))
    print(describe(1, 2))
    print(describe(1.5))
    return
}
"#,
        )
        .is_empty()
    );
}

/// Sharing a name *and* what it takes is redeclaring, not overloading.
#[test]
fn two_declarations_with_the_same_parameters_are_refused() {
    assert_eq!(
        codes(
            r#"
function describe(n: Int) -> Int { return 1 }
function describe(other: Int) -> Int { return 2 }

@Main
function main() {
    print(describe(1))
    return
}
"#,
        ),
        vec!["KSEM003"]
    );
}

/// A call no declaration fits reports what the name expects rather than only
/// that nothing matched.
#[test]
fn a_call_matching_no_overload_reports_the_mismatch() {
    assert_eq!(
        codes(
            r#"
function describe(n: Int) -> Int { return 1 }
function describe(text: String) -> Int { return 2 }

@Main
function main() {
    print(describe(true))
    return
}
"#,
        ),
        vec!["KSEM063"]
    );
}

/// Two declarations that fit a call equally well leave it with no meaning, so
/// the call is refused rather than resolved by declaration order.
#[test]
fn a_call_fitting_two_overloads_equally_is_ambiguous() {
    assert_eq!(
        codes(
            r#"
construct Family { @Required function value() -> Int }
construct Leaf(number: Int) extends Family { function value() -> Int { return number } }

function pick(a: Any Family, b: Leaf) -> Int { return 1 }
function pick(a: Leaf, b: Any Family) -> Int { return 2 }

@Main
function main() {
    print(pick(Leaf(number: 1), Leaf(number: 2)))
    return
}
"#,
        ),
        vec!["KSEM275"]
    );
}

/// An argument that *is* the parameter's type beats one that has to be erased
/// into it, so a concrete declaration reaches the concrete overload.
#[test]
fn a_concrete_argument_prefers_the_concrete_overload() {
    assert!(
        library_codes(
            r#"
construct Family { @Required function value() -> Int }
construct Leaf(number: Int) extends Family { function value() -> Int { return number } }

function size(v: Any Family) -> Int { return v.value() }
function size(v: Leaf) -> Int { return v.value() * 1000 }

function build() -> Int {
    return size(Leaf(number: 3))
}
"#,
        )
        .is_empty()
    );
}

/// Between two declarations that convert equally, the one filling fewer slots
/// from defaults wins, so overloads and defaults compose.
#[test]
fn an_exact_arity_beats_one_that_leans_on_a_default() {
    assert!(
        codes(
            r#"
function describe(n: Int) -> Int { return 1 }
function describe(n: Int, tag: String = "x") -> Int { return 2 }

@Main
function main() {
    print(describe(1))
    print(describe(1, "y"))
    return
}
"#,
        )
        .is_empty()
    );
}

/// A struct's methods overload the way free functions do.
#[test]
fn struct_methods_overload() {
    assert!(
        library_codes(
            r#"
struct Vec2 {
    var x: Int = 0

    function scaled(by: Int) -> Int { return x * by }
    function scaled(by: Int, plus: Int) -> Int { return x * by + plus }
    function scaled(label: String) -> Int { return label.count }
}

function build() -> Int {
    let v = Vec2 { x: 2 }
    return v.scaled(by: 3) + v.scaled(by: 3, plus: 1) + v.scaled(label: "ab")
}
"#,
        )
        .is_empty()
    );
}

/// A construct-backed declaration's members overload too.
#[test]
fn construct_backed_members_overload() {
    assert!(
        library_codes(
            r#"
construct Widget { @Required function total() -> Int }

construct Leaf(number: Int) extends Widget {
    function total() -> Int { return number }
    function scaled(by: Int) -> Int { return number * by }
    function scaled(by: Int, plus: Int) -> Int { return number * by + plus }
}

function build() -> Int {
    let leaf = Leaf(number: 3)
    return leaf.scaled(by: 2) + leaf.scaled(by: 2, plus: 1)
}
"#,
        )
        .is_empty()
    );
}

/// A subclass overriding one overload inherits the rest of them, which is what
/// keying inheritance on the whole member rather than on the name buys.
#[test]
fn overriding_one_overload_leaves_the_others_inherited() {
    assert!(
        library_codes(
            r#"
class Counter {
    var total: Int = 0
    function bump() -> Int { self.total = self.total + 1 return self.total }
    function bump(step: Int) -> Int { self.total = self.total + step return self.total }
}

class Fast extends Counter {
    override function bump(step: Int) -> Int {
        self.total = self.total + step * 100
        return self.total
    }
}

function build() -> Int {
    var f = Fast { }
    return f.bump() + f.bump(step: 2)
}
"#,
        )
        .is_empty()
    );
}

/// An `override` whose parameters match no inherited overload is reported
/// against the name it plainly means rather than as overriding nothing.
#[test]
fn an_override_matching_no_overload_names_the_signature() {
    assert_eq!(
        library_codes(
            r#"
class Counter {
    var total: Int = 0
    function bump(step: Int) -> Int { self.total = self.total + step return self.total }
}

class Odd extends Counter {
    override function bump(step: Int, twice: Bool) -> Int { return 0 }
}
"#,
        ),
        vec!["KSEM066"]
    );
}

/// A construct-backed declaration is constructed through its parenthesized
/// header or through any `init(…)` it declares, and the arguments say which.
#[test]
fn a_construction_reaches_the_header_or_an_init() {
    assert!(
        library_codes(
            r#"
construct Widget { @Required function total() -> Int }

construct Text(text: String) extends Widget {
    function total() -> Int { return text.count }
}

construct Destination(value: Int) extends Widget {
    function total() -> Int { return value }
}

construct NavigationLink(destination: Any Widget, label: Any Widget) extends Widget {
    init(title: String, value: Int) {
        return NavigationLink(
            destination: Destination(value: value),
            label: Text(text: title)
        )
    }

    function total() -> Int { return destination.total() + label.total() }
}

function build() -> Int {
    let header = NavigationLink(destination: Destination(value: 1), label: Text(text: "a"))
    let secondary = NavigationLink(title: "abc", value: 7)
    return header.total() + secondary.total()
}
"#,
        )
        .is_empty()
    );
}

/// An `init` parameter written `some X` takes the construction's trailing
/// children, the way a declaration's child slot does.
#[test]
fn an_init_content_parameter_takes_the_trailing_block() {
    assert!(
        library_codes(
            r#"
construct Widget { @Required function total() -> Int }

construct Text(text: String) extends Widget {
    function total() -> Int { return text.count }
}

construct Link(destination: Int, label: Any Widget) extends Widget {
    init(value: Int, label: some Widget) {
        return Link(destination: value, label: label)
    }

    function total() -> Int { return destination + label.total() }
}

function build() -> Int {
    return Link(value: 5) { Text(text: "hi") }.total()
}
"#,
        )
        .is_empty()
    );
}

/// Content is what a construction writes after its arguments, so an `init` may
/// take it in its last parameter only.
#[test]
fn a_content_parameter_before_a_written_one_is_refused() {
    assert_eq!(
        library_codes(
            r#"
construct Widget { @Required function total() -> Int }

construct Text(text: String) extends Widget {
    function total() -> Int { return text.count }
}

construct Link(destination: Int, label: Any Widget) extends Widget {
    init(label: some Widget, value: Int) {
        return Link(destination: value, label: label)
    }

    function total() -> Int { return destination + label.total() }
}
"#,
        ),
        vec!["KSEM276"]
    );
}

/// An `init` that takes what the header takes leaves a construction fitting
/// both, so it is refused where it is written.
#[test]
fn an_init_shadowing_the_header_is_refused() {
    assert_eq!(
        library_codes(
            r#"
construct Widget { @Required function total() -> Int }

construct Leaf(number: Int) extends Widget {
    init(other: Int) {
        return Leaf(number: other)
    }

    function total() -> Int { return number }
}
"#,
        ),
        vec!["KSEM278"]
    );
}

/// A `{ … }` with no `in` is a closure when the parameter it fills is a
/// function, and children when the callee is a construct-backed declaration.
/// The parser carries both readings; this is the callee choosing.
#[test]
fn a_brace_without_in_is_a_closure_or_content_by_the_callee() {
    assert!(
        codes(
            r#"
function doThing(first: () -> Void, second: () -> Void, third: () -> Void) {
    first()
    second()
    third()
    return
}

construct Widget { @Required function total() -> Int }
construct Text(text: String) extends Widget { function total() -> Int { return text.count } }
construct Row() extends Widget {
    @Content let children: [Any Widget]
    function total() -> Int {
        var sum = 0
        for c in children { sum = sum + c.total() }
        return sum
    }
}

@Main
function main() {
    doThing {
        print("first")
    } second: {
        print("second")
    } third: {
        print("third")
    }
    print(Row { Text(text: "ab") Text(text: "cde") }.total())
    return
}
"#,
        )
        .is_empty()
    );
}

/// A brace on a callee that takes neither children nor a closure is still
/// refused as content, which is what it reads as when nothing else fits.
#[test]
fn a_brace_on_a_callee_wanting_neither_is_still_content() {
    assert_eq!(
        codes(
            r#"
function plain(tag: Int) -> Int {
    return tag
}

@Main
function main() {
    print(plain(tag: 1) { 1 })
    return
}
"#,
        ),
        vec!["KSEM233"]
    );
}

/// A named fill reads as a closure when the parameter it fills is a function,
/// and as content when it fills a child slot.
#[test]
fn a_named_fill_reads_as_a_closure_for_a_function_parameter() {
    assert!(
        codes(
            r#"
function pick(n: Int, ok: () -> Int, other: () -> Int) -> Int {
    if n > 0 { return ok() }
    return other()
}

construct Widget { @Required function total() -> Int }
construct Text(text: String) extends Widget { function total() -> Int { return text.count } }
construct Split() extends Widget {
    let sidebar: some Widget
    let detail: some Widget
    function total() -> Int { return sidebar.total() * 10 + detail.total() }
}

@Main
function main() {
    print(pick(3) { return 1 } other: { return 2 })
    print(Split { Text(text: "ab") } detail: { Text(text: "cde") }.total())
    return
}
"#,
        )
        .is_empty()
    );
}
