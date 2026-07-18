//! The `@Export` surface and every refusal it carries.
//!
//! `@Export` is new Kira design — the oracle has no export concept — so these
//! tests are the specification of what the marker means, not a port of one.
//! Every refusal is checked by code *and* proved to be the only one reported,
//! because a rule that fires alongside a cascade of unrelated errors is not
//! evidence the rule fired for the right reason.

use super::*;

/// The exports a library records, as `(kira name, consumer name)` pairs.
fn exports(text: &str) -> Vec<(String, String)> {
    let db = salsa::DatabaseImpl::new();
    let source = SourceProgram::new(
        &db,
        text.to_owned(),
        "test.kira".to_owned(),
        Vec::new(),
        BuildKind::Library,
    );
    analyzed(&db, source)
        .exports
        .iter()
        .map(|export| (export.kira_name.clone(), export.exported_name.clone()))
        .collect()
}

#[test]
fn a_library_records_its_exports_with_snake_cased_names() {
    let text = "@Export\nfunction makeButton(title: String) -> String { return title }\n\
                @Export\nfunction clickAt(x: Int, y: Int) -> Bool { return x < y }\n\
                function helper(v: Int) -> Int { return v }";
    assert!(
        library_diagnostics(text).is_empty(),
        "{:?}",
        library_codes(text)
    );
    assert_eq!(
        exports(text),
        vec![
            ("makeButton".to_owned(), "make_button".to_owned()),
            ("clickAt".to_owned(), "click_at".to_owned()),
        ],
        "an unmarked function is not exported"
    );
}

#[test]
fn an_export_indexes_the_function_it_names() {
    let text = "function helper(v: Int) -> Int { return v }\n\
                @Export\nfunction add(a: Int, b: Int) -> Int { return a + b }";
    let db = salsa::DatabaseImpl::new();
    let source = SourceProgram::new(
        &db,
        text.to_owned(),
        "test.kira".to_owned(),
        Vec::new(),
        BuildKind::Library,
    );
    let program = analyzed(&db, source);
    assert_eq!(program.exports.len(), 1);
    let export = &program.exports[0];
    assert_eq!(
        program.functions[export.function.0 as usize].name, "add",
        "the export must index the function it names, not the first one"
    );
}

#[test]
fn a_library_that_exports_nothing_has_an_empty_surface() {
    assert!(exports("function add(a: Int) -> Int { return a }").is_empty());
}

#[test]
fn an_export_in_an_application_is_refused_by_name() {
    // The marker is meaningful only where a consumer exists. Checked against
    // the application build kind, which is what a `.App` package analyzes as.
    let text = "@Main function main() { print(1) return }\n\
                @Export\nfunction add(a: Int) -> Int { return a }";
    assert_eq!(codes(text), vec!["KSEM159"], "{:?}", codes(text));
}

#[test]
fn an_export_takes_no_block() {
    let text = "@Export { symbol: uif_add; }\nfunction add(a: Int) -> Int { return a }";
    assert_eq!(
        library_codes(text),
        vec!["KSEM166"],
        "{:?}",
        library_codes(text)
    );
}

#[test]
fn an_export_takes_no_arguments() {
    let text = "@Export(symbol)\nfunction add(a: Int) -> Int { return a }";
    assert_eq!(
        library_codes(text),
        vec!["KSEM166"],
        "{:?}",
        library_codes(text)
    );
}

#[test]
fn a_refused_export_reaches_no_backend() {
    // The refusal is not advisory: a rejected marker records no export, so no
    // engine is ever handed a signature the frontend turned down.
    assert!(exports("@Export(symbol)\nfunction add(a: Int) -> Int { return a }").is_empty());
    assert!(exports("@Export\nfunction titles() -> [String] { return [] }").is_empty());
}

#[test]
fn an_array_cannot_cross_the_boundary() {
    let text = "@Export\nfunction titles() -> [String] { return [] }";
    assert_eq!(
        library_codes(text),
        vec!["KSEM160"],
        "{:?}",
        library_codes(text)
    );
    let taking = "@Export\nfunction count(names: [String]) -> Int { return 0 }";
    assert_eq!(
        library_codes(taking),
        vec!["KSEM160"],
        "a parameter is refused for the same reason a result is"
    );
}

#[test]
fn a_struct_cannot_cross_the_boundary_by_value() {
    let text = "struct Style { let width: Int }\n\
                @Export\nfunction widthOf(s: Style) -> Int { return s.width }";
    assert_eq!(
        library_codes(text),
        vec!["KSEM161"],
        "{:?}",
        library_codes(text)
    );
}

#[test]
fn an_enum_cannot_cross_the_boundary() {
    let text = "enum Color { Red Green }\n\
                @Export\nfunction pick(c: Color) -> Int { return 0 }";
    assert_eq!(
        library_codes(text),
        vec!["KSEM162"],
        "{:?}",
        library_codes(text)
    );
}

#[test]
fn a_function_value_cannot_cross_the_boundary() {
    let text = "@Export\nfunction onClick(handler: (Int) -> Void) { return }";
    assert_eq!(
        library_codes(text),
        vec!["KSEM163"],
        "{:?}",
        library_codes(text)
    );
}

#[test]
fn only_an_exported_class_crosses_as_a_handle() {
    let unmarked = "class Point { var x: Int = 0 }\n\
                    @Export\nfunction place(p: Point) { return }";
    assert_eq!(
        library_codes(unmarked),
        vec!["KSEM164"],
        "{:?}",
        library_codes(unmarked)
    );
    let marked = "@Export\nclass Point { var x: Int = 0 }\n\
                  @Export\nfunction place(p: Point) { return }";
    assert!(
        library_diagnostics(marked).is_empty(),
        "{:?}",
        library_codes(marked)
    );
}

#[test]
fn an_exported_class_may_be_a_result_as_well_as_a_parameter() {
    let text = "@Export\nclass Button { var title: String = \"\" }\n\
                @Export\nfunction makeButton(title: String) -> Button { \
                var b = Button() b.title = title return b }";
    assert!(
        library_diagnostics(text).is_empty(),
        "{:?}",
        library_codes(text)
    );
    assert_eq!(
        exports(text),
        vec![("makeButton".to_owned(), "make_button".to_owned())]
    );
}

#[test]
fn an_unknown_type_on_an_exported_function_is_not_reported_an_extra_time() {
    // The export check reads the types the signature pass already resolved
    // rather than re-resolving what was written. Re-resolving would report the
    // unknown name again, so marking a function `@Export` would make an
    // unrelated typo noisier — a marker must not change how many times an
    // error about something else is reported.
    let plain = "function makeButton(title: Widget) -> String { return \"\" }";
    let exported = "@Export\nfunction makeButton(title: Widget) -> String { return \"\" }";
    let plain_count = library_diagnostics(plain)
        .iter()
        .filter(|diagnostic| diagnostic.code == Some("KSEM050"))
        .count();
    let exported_count = library_diagnostics(exported)
        .iter()
        .filter(|diagnostic| diagnostic.code == Some("KSEM050"))
        .count();
    assert_eq!(
        exported_count,
        plain_count,
        "`@Export` added a KSEM050: {:?}",
        library_diagnostics(exported)
    );
}

#[test]
fn an_export_marker_on_a_class_in_an_application_is_refused() {
    // The class half of the marker obeys the same package rule as the function
    // half — otherwise an application could mint handle-eligible classes for a
    // consumer it does not have.
    let text = "@Main function main() { print(1) return }\n\
                @Export\nclass Button { var title: String = \"\" }";
    assert_eq!(codes(text), vec!["KSEM159"], "{:?}", codes(text));
}

#[test]
fn a_refused_class_marker_does_not_make_the_class_crossable() {
    // Reporting the marker and then honoring it would make the refusal a
    // no-op, so the class stays un-exported and the function using it is
    // refused too.
    let text = "@Export { symbol: btn; }\nclass Button { var title: String = \"\" }\n\
                @Export\nfunction widthOf(b: Button) -> Int { return 0 }";
    let reported = library_codes(text);
    assert!(reported.contains(&"KSEM166"), "{reported:?}");
    assert!(reported.contains(&"KSEM164"), "{reported:?}");
}

#[test]
fn an_exported_parameter_may_not_declare_move() {
    let text = "@Export\nfunction takeTitle(s: move String) { return }";
    assert_eq!(
        library_codes(text),
        vec!["KSEM165"],
        "{:?}",
        library_codes(text)
    );
}

#[test]
fn an_exported_parameter_may_not_declare_borrow_mut() {
    // `borrow mut` collects the standing KSEM112 too: it is unimplemented
    // everywhere, and refused at the boundary for a second, independent
    // reason. Both are real, so both are reported.
    let text = "@Export\nclass Button { var width: Int = 1 }\n\
                @Export\nfunction bump(b: borrow mut Button) { return }";
    let reported = library_codes(text);
    assert!(reported.contains(&"KSEM165"), "{reported:?}");
}

#[test]
fn a_plain_borrow_parameter_is_accepted() {
    // Only the two modes the boundary cannot honor are refused; `borrow` is
    // what a string parameter already is.
    let text = "@Export\nfunction look(s: borrow String) -> Int { return 0 }";
    assert!(
        library_diagnostics(text).is_empty(),
        "{:?}",
        library_codes(text)
    );
}

#[test]
fn a_method_cannot_be_exported() {
    let text = "@Export\nclass Button { var title: String = \"\"\n\
                @Export function label() -> String { return self.title } }";
    assert_eq!(
        library_codes(text),
        vec!["KSEM167"],
        "{:?}",
        library_codes(text)
    );
}

#[test]
fn two_exports_may_not_map_to_one_consumer_name() {
    let text = "@Export\nfunction buttonLabel() -> Int { return 1 }\n\
                @Export\nfunction button_label() -> Int { return 2 }";
    assert_eq!(
        library_codes(text),
        vec!["KSEM168"],
        "{:?}",
        library_codes(text)
    );
    // The first spelling stays valid; only the collision is dropped.
    assert_eq!(
        exports(text),
        vec![("buttonLabel".to_owned(), "button_label".to_owned())]
    );
}

#[test]
fn an_unexported_function_may_still_collide_after_mapping() {
    // The check is about the exported surface, not about Kira names: two
    // functions may share a snake_cased spelling as long as at most one of
    // them is exported.
    let text = "@Export\nfunction buttonLabel() -> Int { return 1 }\n\
                function button_label() -> Int { return 2 }";
    assert!(
        library_diagnostics(text).is_empty(),
        "{:?}",
        library_codes(text)
    );
}

#[test]
fn a_void_export_needs_no_result_type() {
    let text = "@Export\nfunction reset(width: Int) { return }";
    assert!(
        library_diagnostics(text).is_empty(),
        "{:?}",
        library_codes(text)
    );
    assert_eq!(
        exports(text),
        vec![("reset".to_owned(), "reset".to_owned())]
    );
}

#[test]
fn every_scalar_crosses_the_boundary() {
    let text = "@Export\nfunction scalars(a: Int, b: I8, c: U64, d: Float, e: F32, \
                f: Bool, g: String) -> String { return g }";
    assert!(
        library_diagnostics(text).is_empty(),
        "{:?}",
        library_codes(text)
    );
}
