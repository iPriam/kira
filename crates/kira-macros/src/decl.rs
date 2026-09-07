//! Declaration reflection: what a macro sees when it is handed a
//! `Declaration`.
//!
//! This is a *locating* scan, not a parse. `Declaration.syntax` and
//! `Field.syntax` are documented as the declaration's and the field's exact
//! source text, and `Syntax.dropField` / `Syntax.rewriteProperty` are span
//! edits that must leave everything they do not touch byte-for-byte intact,
//! comments included. So every piece here is a byte range of the original file
//! rather than a node, and the annotations the real parser discards — the ones
//! that summon macros — are preserved.

use kira_source::SourceId;
use kira_syntax_model::TokenKind;

use crate::tokens::Lexed;

mod model;
mod scan;

pub(crate) use model::{Annotation, Declaration, DeclarationKind, Field};

use scan::{
    backing_family, scan_annotations, scan_distinct, scan_fields, scan_hooks, scan_members,
    scan_variants, starts_declaration,
};

/// Scans the declaration starting at token `start`, which must sit on the first
/// `@` or on the declaration's first keyword.
///
/// Returns the declaration and the index just past it, or `None` when the file
/// ends inside it.
pub(crate) fn scan(file: &Lexed<'_>, start: usize) -> Option<(Declaration, usize)> {
    let (annotations, head) = scan_annotations(file, start);
    let (kind, name_index) = match file.kind(head) {
        TokenKind::Struct => (DeclarationKind::Struct, head + 1),
        TokenKind::Class => (DeclarationKind::Class, head + 1),
        TokenKind::Enum => (DeclarationKind::Enum, head + 1),
        // One keyword, two forms: a parameter list makes it a declaration
        // backed by the family its `extends` clause names, and its absence
        // makes it the family template.
        TokenKind::Construct => match file.kind(head + 2) {
            TokenKind::LParen => (DeclarationKind::Form, head + 1),
            _ => (DeclarationKind::Construct, head + 1),
        },
        TokenKind::Function => (DeclarationKind::Function, head + 1),
        TokenKind::Distinct => (DeclarationKind::Distinct, head + 1),
        _ => (DeclarationKind::Other, head),
    };
    let name = if file.is_ident(name_index) {
        file.text_at(name_index).to_owned()
    } else {
        String::new()
    };
    // The family a backed declaration is written against, read off its
    // `extends` clause.
    let family = match kind {
        DeclarationKind::Form => backing_family(file, name_index),
        _ => String::new(),
    };

    // A `distinct` declaration has neither a body nor a terminator: it ends at
    // the last token of the representation it names. The generic scan below
    // looks for a `{` or a `;` and would run to the end of the file, taking
    // every declaration under it with it, so this form is located on its own.
    if kind == DeclarationKind::Distinct {
        return scan_distinct(file, head, name_index, name, annotations);
    }

    // Where the declaration ends: at its `{ … }` body, or — for a bodyless
    // `@FFI.Extern` / `@FFI.Syscall` function, which is nothing but a
    // signature — at the token that starts the next declaration, the closing
    // `}` of an enclosing body, or the end of the file.
    //
    // A bodyless declaration that ends the file must still be returned: the
    // caller walking declarations stops at the first `None`, so bailing here
    // would make every such declaration silently stop existing.
    let mut index = head;
    let mut body = None;
    let mut end = None;
    while index < file.len() {
        match file.kind(index) {
            TokenKind::Eof | TokenKind::RBrace => {
                end = Some(index);
                break;
            }
            kind if index > head && starts_declaration(kind) => {
                end = Some(index);
                break;
            }
            TokenKind::LBrace => {
                body = Some((index, file.match_close(index)?));
                break;
            }
            TokenKind::LParen => index = file.match_close(index)?,
            _ => {}
        }
        index += 1;
    }
    let (last, next) = match (body, end) {
        (Some((_, close)), _) => (close, close + 1),
        // A bodyless declaration with nothing after its head is malformed.
        (None, Some(after)) if after == head + 1 => return None,
        (None, Some(after)) => (after - 1, after),
        (None, None) => return None,
    };
    let span = file.span_of(head, last);
    // Everything below reads the *body*, so a bodyless declaration has none of
    // it: no fields to enumerate, no members to inherit, no hooks to run. What it
    // has is its name, its annotations, and its syntax, which is what a collector
    // asks a foreign declaration for.
    let fields = match (kind, body) {
        (DeclarationKind::Enum, Some((open, close))) => scan_variants(file, open, close),
        (_, Some((open, close))) => scan_fields(file, open, close),
        (_, None) => Vec::new(),
    };
    // A backed declaration's members are the bodies it provides; a family's are
    // the defaults a declaration that says nothing inherits. Both are worth
    // running, which is why both are scanned. A struct's `function` is not: it
    // is a method with a receiver the evaluator has no value for.
    let members = match (kind, body) {
        (DeclarationKind::Form | DeclarationKind::Construct, Some((open, close))) => {
            scan_members(file, open, close)
        }
        _ => Vec::new(),
    };
    let hooks = match (kind, body) {
        (DeclarationKind::Construct, Some((open, close))) => scan_hooks(file, open, close),
        _ => Vec::new(),
    };

    Some((
        Declaration {
            kind,
            name,
            family,
            fields,
            members,
            hooks,
            syntax: file.slice(span).to_owned(),
            span,
            source: Some(file.source),
            path: file.path.clone(),
            line: file.line_of(span.start),
            file_lines: file.line_count(),
            annotations,
        },
        next,
    ))
}

/// Re-scans `text` as a standalone declaration.
///
/// This is what makes `Syntax` closed under the declaration-shaped operations:
/// `dropField` and `rewriteProperty` both return syntax that a further
/// `dropField` may be applied to, and both return *text*, so the way to answer
/// "is this still a declaration?" is to look at it again.
pub(crate) fn parse(text: &str) -> Option<Declaration> {
    let file = Lexed::new(SourceId::new(0), text);
    let mut index = 0usize;
    while index < file.len() && file.kind(index) == TokenKind::Eof {
        index += 1;
    }
    let (mut declaration, _) = scan(&file, 0)?;
    if declaration.kind == DeclarationKind::Other {
        return None;
    }
    // Detached text: the spans are offsets into `text`, not into any file, so
    // the file they came from is unknown rather than [`SourceId::new(0)`], which
    // is a real id belonging to a real file. See [`Declaration::source`].
    declaration.source = None;
    declaration.path = std::sync::Arc::from("");
    declaration.line = 0;
    declaration.file_lines = 0;
    for field in &mut declaration.fields {
        field.source = None;
    }
    Some(declaration)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan_text(text: &str) -> Declaration {
        let file = Lexed::new(SourceId::new(0), text);
        scan(&file, 0).expect("a declaration").0
    }

    /// A bodyless declaration ends at its `;`, and the scan goes on past it.
    ///
    /// This is what a collector's view of a file depends on: the walk stops at
    /// the first declaration it cannot scan, so an `@FFI.Extern` that swallowed
    /// the rest of the file took every `Test` written below it with it — with no
    /// diagnostic, because nothing had gone wrong as far as anything could tell.
    #[test]
    fn a_bodyless_foreign_declaration_does_not_swallow_the_file_below_it() {
        let text = "@FFI.Syscall { name: write }
             function sysWrite(fd: Int, buffer: CString, count: U64) -> Int
             @FFI.Extern { library: l, symbol: s, abi: c }
             function add(a: I32) -> I32
             struct Point {
    var x: Int
}
";
        let file = Lexed::new(SourceId::new(0), text);
        let found = crate::procedural::top_level(&file);
        let names: Vec<&str> = found
            .iter()
            .map(|declaration| declaration.name.as_str())
            .collect();
        assert_eq!(names, vec!["sysWrite", "add", "Point"]);
        // The bodyless ones have a name, their annotations, and their syntax, and
        // no body to enumerate.
        assert_eq!(found[0].kind, DeclarationKind::Function);
        assert!(found[0].fields.is_empty());
        assert_eq!(found[0].annotations.len(), 1);
        assert_eq!(found[0].annotations[0].name, "FFI.Syscall");
        assert!(found[0].syntax.starts_with("function sysWrite"));
        assert_eq!(found[1].annotations[0].name, "FFI.Extern");
        // The declaration *after* them is scanned whole, which is the part that
        // was lost.
        assert_eq!(found[2].fields.len(), 1);
    }

    #[test]
    fn a_struct_reflects_its_fields() {
        let declaration = scan_text("struct Point {\n    var x: Int\n    var y: Int = 2\n}\n");
        assert_eq!(declaration.kind, DeclarationKind::Struct);
        assert_eq!(declaration.name, "Point");
        assert_eq!(declaration.fields.len(), 2);
        assert_eq!(declaration.fields[0].name, "x");
        assert_eq!(declaration.fields[0].type_text, "Int");
        assert_eq!(declaration.fields[0].initializer, "");
        assert_eq!(declaration.fields[1].initializer, "2");
    }

    #[test]
    fn an_enum_reflects_its_variants() {
        let declaration = scan_text("enum Color {\n    Red\n    Green\n    Blue\n}\n");
        assert_eq!(declaration.kind, DeclarationKind::Enum);
        let names: Vec<&str> = declaration
            .fields
            .iter()
            .map(|field| field.name.as_str())
            .collect();
        assert_eq!(names, vec!["Red", "Green", "Blue"]);
    }

    #[test]
    fn a_payload_variant_carries_its_type() {
        let declaration = scan_text("enum Outcome {\n    Ok: Int\n    Error: AppError\n}\n");
        assert_eq!(declaration.fields[0].type_text, "Int");
        assert_eq!(declaration.fields[1].type_text, "AppError");
    }

    #[test]
    fn a_parenthesized_payload_is_the_variants_type_not_a_variant() {
        // The `Name(Type)` form went unscanned, so the payload type came back as
        // a variant of its own and every derive emitted an arm for it.
        let declaration = scan_text("enum Note {\n    Blank\n    Rank(Int)\n    Tag(String)\n}\n");
        let names: Vec<&str> = declaration
            .fields
            .iter()
            .map(|field| field.name.as_str())
            .collect();
        assert_eq!(names, vec!["Blank", "Rank", "Tag"]);
        assert_eq!(declaration.fields[0].type_text, "");
        assert_eq!(declaration.fields[1].type_text, "Int");
        assert_eq!(declaration.fields[2].type_text, "String");
    }

    #[test]
    fn an_empty_payload_list_is_a_payload_less_variant() {
        let declaration = scan_text("enum Flag {\n    On()\n    Off\n}\n");
        assert_eq!(declaration.fields.len(), 2);
        assert_eq!(declaration.fields[0].name, "On");
        assert_eq!(declaration.fields[0].type_text, "");
    }

    #[test]
    fn a_field_annotation_is_preserved() {
        let declaration = scan_text(
            "construct Demo() extends MfxPanel {\n    @Tracked var count: Int = 7\n    let body: Int = 1\n}\n",
        );
        assert_eq!(declaration.kind, DeclarationKind::Form);
        assert_eq!(declaration.name, "Demo");
        assert_eq!(declaration.fields[0].annotations[0].name, "Tracked");
        assert_eq!(declaration.fields[0].initializer, "7");
        assert!(declaration.fields[0].syntax.starts_with("@Tracked"));
        assert!(declaration.fields[1].annotations.is_empty());
    }

    #[test]
    fn a_form_field_stops_before_a_named_rule_body() {
        let declaration = scan_text(
            "construct DashboardShell() extends Widget {\n                @State var status: String = \"ready\"\n\n                body {\n                    Text(status)\n                }\n            }",
        );

        assert_eq!(declaration.fields.len(), 1);
        assert_eq!(declaration.fields[0].name, "status");
        assert_eq!(declaration.fields[0].initializer, "\"ready\"");
        assert!(!declaration.fields[0].syntax.contains("body {"));
        assert_eq!(declaration.members.len(), 1);
        assert_eq!(declaration.members[0].name, "body");
        assert!(declaration.members[0].body.contains("Text(status)"));
    }

    #[test]
    fn a_form_field_keeps_a_braced_initializer_before_a_named_rule() {
        let declaration = scan_text(
            "construct DashboardShell() extends Widget {\n                @State var model: Model = Model { value: 1 }\n\n                body {\n                    Text(\"ready\")\n                }\n            }",
        );

        assert_eq!(declaration.fields.len(), 1);
        assert!(
            declaration.fields[0]
                .initializer
                .contains("Model { value: 1 }")
        );
        assert_eq!(declaration.members.len(), 1);
        assert_eq!(declaration.members[0].name, "body");
    }

    #[test]
    fn a_declaration_annotation_is_separate_from_its_syntax() {
        let declaration =
            scan_text("@Derive(Equatable, Clone)\nstruct Point {\n    var x: Int\n}\n");
        assert_eq!(declaration.annotations.len(), 1);
        assert_eq!(declaration.annotations[0].name, "Derive");
        assert_eq!(
            declaration.annotations[0].arguments,
            vec!["Equatable", "Clone"]
        );
        assert!(declaration.syntax.starts_with("struct Point"));
    }

    #[test]
    fn methods_are_not_fields() {
        let declaration = scan_text(
            "struct S {\n    var a: Int\n    function get() -> Int {\n        var hidden = 1\n        return hidden\n    }\n    var b: Int\n}\n",
        );
        let names: Vec<&str> = declaration
            .fields
            .iter()
            .map(|field| field.name.as_str())
            .collect();
        assert_eq!(names, vec!["a", "b"]);
    }

    #[test]
    fn an_untyped_field_reports_its_initializer_without_the_equals() {
        // `let enabled = true` has no written type, so the `=` is the first
        // token after the name — and the initializer is what follows it, not
        // the assignment itself.
        let declaration = scan_text(
            "construct Entry() extends Lint {\n    let enabled = true\n    let code = \"K1\"\n}\n",
        );
        assert_eq!(declaration.fields[0].type_text, "");
        assert_eq!(declaration.fields[0].initializer, "true");
        assert_eq!(declaration.fields[1].initializer, "\"K1\"");
    }

    #[test]
    fn syntax_round_trips_through_a_reparse() {
        let text = "struct Point {\n    var x: Int = 1 // a comment\n}\n";
        let declaration = scan_text(text);
        let again = parse(&declaration.syntax).expect("a declaration");
        assert_eq!(again.name, "Point");
        assert_eq!(again.fields.len(), 1);
        assert!(again.syntax.contains("// a comment"));
    }
}
