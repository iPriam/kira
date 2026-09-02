//! Documentation extraction and Markdown rendering for Kira packages.
//!
//! Layer 8 of the Kira package graph. Comments are intentionally read from the
//! original source text because the lexer treats them as trivia; declaration
//! identity and spans still come from the shared parser tree.

use kira_core::Names;
use kira_parser::ParseResult;
use kira_source::{FileSpan, SourceId, Span};
use kira_syntax_model::ast::Item;

/// One documented top-level declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocItem {
    /// The declaration category (`function`, `struct`, …).
    pub kind: String,
    /// The source-level declaration name.
    pub name: String,
    /// The first declaration line, useful as a compact API signature.
    pub signature: String,
    /// Consecutive `///` lines immediately preceding the declaration.
    pub description: String,
    /// The declaration's source span.
    pub span: FileSpan,
}

/// Extracts documented declarations from one parsed source file.
#[must_use]
pub fn collect(source: SourceId, text: &str, parsed: &ParseResult) -> Vec<DocItem> {
    parsed
        .tree
        .items()
        .iter()
        .filter_map(|item| item_metadata(item, &parsed.interner))
        .map(|(kind, name, span)| DocItem {
            kind,
            name,
            signature: signature(text, span),
            description: docs_before(text, span.start),
            span: FileSpan::new(source, span),
        })
        .collect()
}

/// Renders extracted API documentation as deterministic Markdown.
#[must_use]
pub fn render_markdown(package: &str, items: &[DocItem]) -> String {
    let mut output = format!("# {package}\n\n");
    for item in items {
        output.push_str(&format!("## {} `{}`\n\n", item.kind, item.name));
        if !item.description.is_empty() {
            output.push_str(&item.description);
            output.push_str("\n\n");
        }
        if !item.signature.is_empty() {
            output.push_str("```kira\n");
            output.push_str(&item.signature);
            output.push_str("\n```\n\n");
        }
    }
    output
}

fn item_metadata(item: &Item, names: &Names) -> Option<(String, String, Span)> {
    match item {
        Item::Function(function) => Some((
            "function".to_owned(),
            names.resolve(function.name).to_owned(),
            function.span,
        )),
        Item::Struct(declaration) => Some((
            "struct".to_owned(),
            names.resolve(declaration.name).to_owned(),
            declaration.span,
        )),
        Item::Class(declaration) => Some((
            "class".to_owned(),
            names.resolve(declaration.name).to_owned(),
            declaration.span,
        )),
        Item::Enum(declaration) => Some((
            "enum".to_owned(),
            names.resolve(declaration.name).to_owned(),
            declaration.span,
        )),
        Item::TypeAlias(declaration) => Some((
            "type".to_owned(),
            names.resolve(declaration.name).to_owned(),
            declaration.span,
        )),
        Item::Distinct(declaration) => Some((
            "distinct".to_owned(),
            names.resolve(declaration.name).to_owned(),
            declaration.span,
        )),
        Item::Constant(declaration) => Some((
            "let".to_owned(),
            names.resolve(declaration.name).to_owned(),
            declaration.span,
        )),
        Item::Construct(declaration) => Some((
            "construct".to_owned(),
            names.resolve(declaration.name).to_owned(),
            declaration.span,
        )),
        Item::Extend(declaration) => Some((
            "extend".to_owned(),
            names.resolve(declaration.name).to_owned(),
            declaration.span,
        )),
        Item::Trait(declaration) => Some((
            "trait".to_owned(),
            names.resolve(declaration.name).to_owned(),
            declaration.span,
        )),
        Item::Import(_) | Item::Unsupported(_) => None,
    }
}

fn docs_before(text: &str, start: u32) -> String {
    let prefix = &text[..start as usize];
    let mut lines = Vec::new();
    let mut allow_annotations = true;
    for line in prefix.lines().rev() {
        let trimmed = line.trim();
        if trimmed.is_empty() && lines.is_empty() {
            continue;
        }
        if allow_annotations && trimmed.starts_with('@') {
            continue;
        }
        if let Some(doc) = trimmed.strip_prefix("///") {
            lines.push(doc.strip_prefix(' ').unwrap_or(doc).to_owned());
            allow_annotations = false;
            continue;
        }
        break;
    }
    lines.reverse();
    lines.join("\n")
}

fn signature(text: &str, span: Span) -> String {
    let snippet = span.slice(text);
    snippet
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('@'))
        .map(|line| {
            line.split_once('{')
                .map_or(line, |(head, _)| head.trim_end())
        })
        .unwrap_or_default()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_doc_comments_and_keeps_parser_spans() {
        let text = "/// Adds two numbers.\n/// The result is exact.\nfunction add(a: Int, b: Int) -> Int { return a + b }\n";
        let source = SourceId::new(4);
        let parsed = kira_parser::parse(source, text);
        let items = collect(source, text, &parsed);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "add");
        assert_eq!(
            items[0].description,
            "Adds two numbers.\nThe result is exact."
        );
        assert_eq!(items[0].span.source, source);
        assert_eq!(items[0].signature, "function add(a: Int, b: Int) -> Int");
    }

    #[test]
    fn annotations_do_not_break_the_documentation_attachment() {
        let text = "/// Entrypoint.\n@Main\nfunction main() { return }\n";
        let parsed = kira_parser::parse(SourceId::new(0), text);
        let items = collect(SourceId::new(0), text, &parsed);
        assert_eq!(items[0].description, "Entrypoint.");
        assert!(render_markdown("demo", &items).contains("# demo"));
    }
}
