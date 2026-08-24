//! User `Drop`: what a claim obliges, and every seam that refuses a value
//! running a body.
//!
//! The guarantee: **every user `Drop` body runs before the run that made the
//! value ends**, exactly once per value. Nothing can enter a body where no
//! engine is executing, so a type that runs one is refused at each position
//! that outlives the run, and at each read that would give one value's storage
//! two owners.

use super::*;

const MAIN: &str = "@Main function main() { return }\n";

/// A program whose declarations and `@Main` are the whole file.
fn program(body: &str) -> String {
    format!("{body}{MAIN}")
}

/// A `Drop` type, a pair holding two of them, and a function that borrows one.
const TRACING: &str = "struct D: Drop {\n    var tag: Int\n\
                       \n    function drop(borrow mut self) { return }\n\
                       \n    function bump(borrow mut self) { self.tag = self.tag + 1 return }\n\
                       \n    function value(borrow self) -> Int { return self.tag }\n}\n\
                       struct Pair {\n    var first: D\n    var second: D\n}\n\
                       function read(held: borrow D) -> Int { return held.tag }\n";

/// The codes of a program whose `@Main` runs `body`, with [`TRACING`] declared.
fn tracing_codes(body: &str) -> Vec<String> {
    codes(&format!(
        "{TRACING}@Main function main() {{\n{body}\n    return\n}}\n"
    ))
}

const DROPPING: &str = "struct Handle: Drop {\n    let id: Int\n\
                        \n    function drop(borrow mut self) { return }\n}\n";

#[test]
fn a_drop_conformance_with_a_drop_member_is_accepted() {
    let items = diagnostics(&program(DROPPING));
    assert!(items.is_empty(), "{items:?}");
}

#[test]
fn a_drop_conformance_with_no_drop_member_is_refused() {
    let items = diagnostics(&program("struct Handle: Drop {\n    let id: Int\n}\n"));
    let refusal = items
        .iter()
        .find(|item| item.has_code("KSEM301"))
        .unwrap_or_else(|| panic!("expected a KSEM301, got {items:?}"));
    assert!(
        refusal.message.contains("presents no `drop`"),
        "{refusal:?}"
    );
}

#[test]
fn a_drop_member_that_takes_or_returns_anything_is_refused() {
    let items = diagnostics(&program(
        "struct Handle: Drop {\n    let id: Int\n\
         \n    function drop(borrow mut self, extra: Int) { return }\n}\n",
    ));
    let codes: Vec<String> = items
        .iter()
        .filter_map(Diagnostic::code_text)
        .map(str::to_owned)
        .collect();
    assert!(codes.contains(&"KSEM301".to_owned()), "{items:?}");
}

#[test]
fn calling_drop_by_name_is_refused() {
    let items = diagnostics(&format!(
        "{DROPPING}@Main function main() {{ let h = Handle(id: 1) h.drop() return }}\n"
    ));
    let refusal = items
        .iter()
        .find(|item| item.has_code("KSEM300"))
        .unwrap_or_else(|| panic!("expected a KSEM300, got {items:?}"));
    assert!(
        refusal.message.contains("run by the release"),
        "{refusal:?}"
    );
}

#[test]
fn a_drop_type_is_not_copyable() {
    let items = diagnostics(&program(
        "struct Handle: Copyable, Drop {\n    let id: Int\n\
         \n    function drop(borrow mut self) { return }\n}\n",
    ));
    let refusal = items
        .iter()
        .find(|item| item.has_code("KSEM297"))
        .unwrap_or_else(|| panic!("expected a KSEM297, got {items:?}"));
    assert!(refusal.message.contains("`Drop`"), "{refusal:?}");
}

#[test]
fn a_drop_type_moves_when_it_is_bound() {
    let items = diagnostics(&format!(
        "{DROPPING}@Main function main() {{ let a = Handle(id: 1) let b = a print(a.id) return }}\n"
    ));
    assert!(!items.is_empty(), "binding a `Drop` value moves it");
}

#[test]
fn a_drop_body_is_registered_as_the_types_glue() {
    let program = analyze_text(&program(DROPPING));
    let id = program
        .types
        .structs()
        .lookup("Handle")
        .expect("the struct is declared");
    let def = program.types.structs().get(id).expect("the id resolves");
    assert!(def.drop_glue.is_some(), "the body is recorded on the type");
    // A type that runs a body is released wherever it is held, even when every
    // member it holds is a scalar.
    assert!(program.types.owns_heap(Type::Struct(id)));
    assert!(program.types.moves_on_bind(Type::Struct(id)));
}

#[test]
fn a_type_holding_a_drop_value_runs_one_too() {
    let program = analyze_text(&program(&format!(
        "{DROPPING}struct Box {{\n    let held: Handle\n}}\n"
    )));
    let id = program
        .types
        .structs()
        .lookup("Box")
        .expect("the struct is declared");
    assert!(program.types.runs_user_drop(Type::Struct(id)));
    assert!(program.types.moves_on_bind(Type::Struct(id)));
}

#[test]
fn an_impl_block_for_drop_may_declare_only_drop() {
    let items = diagnostics(&program(
        "struct Handle {\n    let id: Int\n}\n\
         extend Handle: Drop {\n    function drop(borrow mut self) { return }\n\
         \n    function extra(borrow self) -> Int { return 1 }\n}\n",
    ));
    let codes: Vec<String> = items
        .iter()
        .filter_map(Diagnostic::code_text)
        .map(str::to_owned)
        .collect();
    assert!(codes.contains(&"KSEM294".to_owned()), "{items:?}");
}

// ----- KSEM302: a read that takes the value out of its owner ---------------

#[test]
fn a_member_read_that_takes_the_value_is_refused() {
    let codes = tracing_codes(
        "    var pair = Pair(first: D(tag: 1), second: D(tag: 2))\n\
         \x20   let stolen = pair.first",
    );
    assert_eq!(codes, vec!["KSEM302"]);
}

#[test]
fn an_element_read_that_takes_the_value_is_refused() {
    let codes = tracing_codes(
        "    let arr: [D] = [D(tag: 1), D(tag: 2)]\n\
         \x20   let taken = arr[1]",
    );
    assert_eq!(codes, vec!["KSEM302"]);
}

/// Three positions read a member without owning it, and each one leaves the
/// container holding its value: the base of a further read, an argument a
/// callee borrows, and the receiver of a method that does not mutate it.
#[test]
fn a_member_read_no_position_owns_is_accepted() {
    let codes = tracing_codes(
        "    let pair = Pair(first: D(tag: 1), second: D(tag: 2))\n\
         \x20   print(pair.first.tag)\n\
         \x20   print(read(pair.first))\n\
         \x20   print(pair.first.value())\n\
         \x20   let arr: [D] = [D(tag: 1)]\n\
         \x20   print(arr.count)\n\
         \x20   print(arr[0].tag)",
    );
    assert_eq!(codes, Vec::<String>::new());
}

/// A mutating method is the opposite of a borrowing one: it takes the receiver
/// by value and writes it back, which for a member is a second owner.
#[test]
fn a_mutating_method_on_a_member_is_refused() {
    let codes = tracing_codes(
        "    var pair = Pair(first: D(tag: 1), second: D(tag: 2))\n\
         \x20   pair.first.bump()",
    );
    assert_eq!(codes, vec!["KSEM302"]);
}

/// The cursor binds one element per iteration while the array still holds it.
#[test]
fn a_for_cursor_over_drop_elements_is_refused() {
    let codes = tracing_codes(
        "    let arr: [D] = [D(tag: 1)]\n\
         \x20   for item in arr {\n        print(item.tag)\n    }",
    );
    assert_eq!(codes, vec!["KSEM302"]);
}

/// A read out of a value the expression itself computed cannot be excused by
/// any enclosing position: that value is released at the end of the statement,
/// so the read would be a second one with the same body to run.
#[test]
fn a_member_read_out_of_a_temporary_is_refused() {
    let codes = codes(&format!(
        "{TRACING}function make() -> Pair {{ return Pair(first: D(tag: 1), second: D(tag: 2)) }}\n\
         @Main function main() {{ print(make().first.tag) return }}\n"
    ));
    assert_eq!(codes, vec!["KSEM302"]);
}

/// A captured `var` is shared with the scope that declared it, and every read
/// out of that share is a value of what the share holds.
#[test]
fn a_closure_capture_of_a_drop_value_is_refused() {
    let codes = tracing_codes(
        "    var held = D(tag: 1)\n\
         \x20   let peek: () -> Int = { in return held.tag }\n\
         \x20   print(peek())",
    );
    assert_eq!(codes, vec!["KSEM302"]);
}

// ----- KSEM303: the `@Export` boundary ------------------------------------

#[test]
fn an_exported_result_that_runs_a_body_is_refused() {
    let codes = library_codes(&format!(
        "{TRACING}@Export\nclass Handle: Drop {{\n    var held: Int = 0\n\
         \n    function drop(borrow mut self) {{ return }}\n}}\n\
         @Export\nfunction open() -> Handle {{ return Handle() }}\n"
    ));
    assert_eq!(codes, vec!["KSEM303"]);
}

#[test]
fn an_exported_parameter_that_runs_a_body_is_refused() {
    let codes = library_codes(&format!(
        "{TRACING}@Export\nclass Handle: Drop {{\n    var held: Int = 0\n\
         \n    function drop(borrow mut self) {{ return }}\n}}\n\
         @Export\nfunction close(handle: Handle) {{ return }}\n"
    ));
    assert_eq!(codes, vec!["KSEM303"]);
}

// ----- KSEM304: callback state --------------------------------------------

#[test]
fn native_state_cannot_box_a_value_that_runs_a_body() {
    let codes = tracing_codes("    var state = nativeState(D { tag: 0 })");
    assert_eq!(codes, vec!["KSEM304"]);
}

#[test]
fn native_recover_cannot_name_a_type_that_runs_a_body() {
    let codes = codes(&format!(
        "{TRACING}struct Counter {{ var n: Int }}\n\
         @Main function main() {{\n\
         \x20   var state = nativeState(Counter {{ n: 0 }})\n\
         \x20   var view = nativeRecover<D>(nativeUserData(state))\n\
         \x20   nativeStateFree(state)\n\
         \x20   return\n}}\n"
    ));
    assert_eq!(codes, vec!["KSEM304"]);
}

// ----- KSEM305: a `retains:` foreign parameter ----------------------------

#[test]
fn a_retained_foreign_argument_may_not_run_a_body() {
    let codes = codes(
        "@FFI.Struct { layout: c; }\n\
         struct Handle: Drop {\n    var id: I32\n\
         \n    function drop(borrow mut self) { return }\n}\n\
         @FFI.Extern { library: fixture; symbol: keep; abi: c; retains: value; }\n\
         function keep(value: Handle): Void;\n\
         @Main function main() { let h = Handle { id: 1 } keep(move h) return }\n",
    );
    assert_eq!(codes, vec!["KSEM305"]);
}

// ----- KSEM306: an enum payload -------------------------------------------

#[test]
fn an_enum_payload_that_runs_a_body_is_refused() {
    let codes = codes(&format!(
        "{TRACING}enum Slot {{\n    Full(D)\n    Empty\n}}\n{MAIN}"
    ));
    assert_eq!(codes, vec!["KSEM306"]);
}

/// The instantiation is what runs a body, so the refusal is at the use site
/// rather than at the generic declaration.
#[test]
fn a_generic_enum_instantiated_with_a_drop_type_is_refused() {
    let codes = codes(&format!(
        "{TRACING}enum Slot<T> {{\n    Full(T)\n    Empty\n}}\n\
         @Main function main() {{ let s: Slot<D> = .Empty return }}\n"
    ));
    assert_eq!(codes, vec!["KSEM306"]);
}

/// A backed declaration's family value carries it as a payload, so the payload
/// rule reaches a declaration whose family nobody wrote by hand.
#[test]
fn a_construct_backed_declaration_that_runs_a_body_is_refused() {
    let codes = codes(&format!(
        "{TRACING}construct Style {{\n    let handle: D = D(tag: 0)\n}}\n\
         construct Base() extends Style {{\n    let handle = D(tag: 1)\n}}\n{MAIN}"
    ));
    assert_eq!(codes, vec!["KSEM306"]);
}

// ----- KSEM307: an engine boundary ----------------------------------------

/// A release happens wherever the value dies, so the body is compiled into
/// both halves of a hybrid build. Choosing an engine for it would leave the
/// other half with a release it cannot run.
#[test]
fn a_drop_body_may_not_declare_an_engine() {
    let items = diagnostics(&program(
        "struct Handle {\n    let id: Int\n}\n\
         extend Handle: Drop {\n    @Native\n    function drop(borrow mut self) { return }\n}\n",
    ));
    let refusal = items
        .iter()
        .find(|item| item.has_code("KSEM301"))
        .unwrap_or_else(|| panic!("expected a KSEM301, got {items:?}"));
    assert!(refusal.message.contains("@Native"), "{refusal:?}");
}

#[test]
fn a_drop_value_may_not_cross_between_engines() {
    let result = codes(&format!(
        "{TRACING}@Native\nfunction make(tag: Int) -> D {{ return D(tag: tag) }}\n{MAIN}"
    ));
    assert_eq!(result, vec!["KSEM307"]);

    let parameter = codes(&format!(
        "{TRACING}@Runtime\nfunction take(held: move D) -> Int {{ return held.tag }}\n{MAIN}"
    ));
    assert_eq!(parameter, vec!["KSEM307"]);
}

/// A value built and released inside one half never crosses, so the engine
/// annotation says nothing about it.
#[test]
fn a_drop_value_held_inside_one_engine_is_accepted() {
    let codes = codes(&format!(
        "{TRACING}@Native\nfunction scope(tag: Int) -> Int {{\n\
         \x20   let held = D(tag: tag)\n    return held.tag\n}}\n{MAIN}"
    ));
    assert_eq!(codes, Vec::<String>::new());
}

/// `@Derive(Copy)` asks the same question the claim does, and gets the same
/// answer through the field walk: a type that reaches a body copies nothing.
#[test]
fn deriving_copy_on_a_type_reaching_a_drop_is_refused() {
    let items = diagnostics(&program(&format!(
        "{DROPPING}@Derive(Copy)\nstruct Box {{\n    let held: Handle\n}}\n"
    )));
    let refusal = items
        .iter()
        .find(|item| item.has_code("KIR005"))
        .unwrap_or_else(|| panic!("expected a KIR005, got {items:?}"));
    assert!(refusal.message.contains("Handle"), "{refusal:?}");
}
