//! Go-to-definition links: every jump is recorded by the resolution that
//! type-checked the reference, so these drive the same `analyzed` query the
//! LSP serves them from.

use super::*;
use kira_source::{FileSpan, Span};

/// The links of a program built from an entry file plus named modules.
fn links(text: &str, modules: &[(&str, &str)]) -> Vec<DefinitionLink> {
    let db = salsa::DatabaseImpl::new();
    let modules: Vec<ModuleSource> = modules
        .iter()
        .map(|&(module, text)| ModuleSource {
            module: module.to_owned(),
            path: format!("{module}.kira"),
            text: text.to_owned(),
        })
        .collect();
    let source = SourceProgram::application(&db, text.to_owned(), "test.kira".to_owned(), modules);
    analyzed::accumulated::<DefinitionAccumulator>(&db, source)
        .into_iter()
        .map(|accumulator| accumulator.0)
        .collect()
}

/// The span of the `occurrence`-th appearance of `needle` in `text` (0-based).
fn span_of(text: &str, needle: &str, occurrence: usize) -> Span {
    let mut from = 0;
    for _ in 0..occurrence {
        let found = text[from..].find(needle).expect("occurrence exists");
        from += found + needle.len();
    }
    let start = from + text[from..].find(needle).expect("occurrence exists");
    Span::new(start as u32, needle.len() as u32)
}

/// The link whose reference is exactly `span` in the entry file.
fn link_at(links: &[DefinitionLink], span: Span) -> Option<DefinitionLink> {
    links
        .iter()
        .copied()
        .find(|link| link.reference == FileSpan::new(FILE_SOURCE_ID, span))
}

#[test]
fn a_local_read_links_to_its_binding() {
    let text = "@Main function main() { let value = 1 print(value) return }";
    let all = links(text, &[]);
    let read = span_of(text, "value", 1);
    let binding = span_of(text, "value", 0);
    let link = link_at(&all, read).expect("a local read records a link");
    assert_eq!(link.definition, FileSpan::new(FILE_SOURCE_ID, binding));
}

#[test]
fn a_parameter_read_links_to_the_parameter() {
    let text = "function double(amount: Int) -> Int { return amount * 2 }\n\
                @Main function main() { print(double(3)) return }";
    let all = links(text, &[]);
    let read = span_of(text, "amount", 1);
    let link = link_at(&all, read).expect("a parameter read records a link");
    assert_eq!(
        link.definition,
        FileSpan::new(FILE_SOURCE_ID, span_of(text, "amount", 0))
    );
}

#[test]
fn a_call_links_to_the_function_declaration() {
    let text = "function helper() -> Int { return 7 }\n\
                @Main function main() { print(helper()) return }";
    let all = links(text, &[]);
    let call = span_of(text, "helper", 1);
    let declaration = span_of(text, "helper", 0);
    let link = link_at(&all, call).expect("a call records a link");
    assert_eq!(link.definition, FileSpan::new(FILE_SOURCE_ID, declaration));
}

#[test]
fn a_type_name_links_to_the_struct_declaration() {
    let text = "struct Point { let x: Int }\n\
                function take(p: Point) -> Int { return p.x }\n\
                @Main function main() { print(take(Point { x: 4 })) return }";
    let all = links(text, &[]);
    let annotation = span_of(text, "Point", 1);
    let declaration = span_of(text, "Point", 0);
    let link = link_at(&all, annotation).expect("a written type name records a link");
    assert_eq!(link.definition, FileSpan::new(FILE_SOURCE_ID, declaration));

    // The struct literal's name is a reference too.
    let literal = span_of(text, "Point", 2);
    let link = link_at(&all, literal).expect("a struct literal records a link");
    assert_eq!(link.definition, FileSpan::new(FILE_SOURCE_ID, declaration));
}

#[test]
fn a_field_access_links_to_the_field_declaration() {
    let text = "struct Point { let x: Int }\n\
                @Main function main() { let p = Point { x: 4 } print(p.x) return }";
    let all = links(text, &[]);
    let access = span_of(text, "x", 2);
    let declaration = span_of(text, "x", 0);
    let link = link_at(&all, access).expect("a field access records a link");
    assert_eq!(link.definition, FileSpan::new(FILE_SOURCE_ID, declaration));
}

#[test]
fn an_enum_variant_links_to_the_variant_declaration() {
    let text = "enum Color { Red Green }\n\
                @Main function main() { let c: Color = .Green print(c == .Red) return }";
    let all = links(text, &[]);
    let written = span_of(text, "Green", 1);
    let declaration = span_of(text, "Green", 0);
    let link = link_at(&all, written).expect("a leading-dot variant records a link");
    assert_eq!(link.definition, FileSpan::new(FILE_SOURCE_ID, declaration));
}

#[test]
fn a_cross_module_call_links_into_the_module() {
    let module = "function supportValue() -> Int { return 42 }";
    let text = "import support as Support\n\
                @Main function main() { print(Support.supportValue()) return }";
    let all = links(text, &[("support", module)]);
    let call = span_of(text, "supportValue", 0);
    let link = link_at(&all, call).expect("a cross-module call records a link");
    assert_eq!(
        link.definition,
        FileSpan::new(module_source_id(0), span_of(module, "supportValue", 0)),
        "the definition lands in the module's file"
    );
}

#[test]
fn an_import_links_to_the_top_of_the_module_file() {
    let text = "import support\n@Main function main() { print(1) return }";
    let all = links(
        text,
        &[("support", "function unused() -> Int { return 1 }")],
    );
    let path = span_of(text, "support", 0);
    let link = link_at(&all, path).expect("a resolved import records a link");
    assert_eq!(
        link.definition,
        FileSpan::new(module_source_id(0), Span::new(0, 0))
    );
}

#[test]
fn an_unresolved_name_records_no_link() {
    let text = "@Main function main() { print(missing) return }";
    let all = links(text, &[]);
    let written = span_of(text, "missing", 0);
    assert!(
        link_at(&all, written).is_none(),
        "an unresolved name has no definition to link"
    );
}
