//! Editor requests built on the compiler's semantic links.
//!
//! Diagnostics and navigation already use the frontend directly. This module
//! keeps the richer editor requests beside them without making the protocol
//! loop know how symbols are represented: hover follows a resolved link, and
//! completion presents names declared in the current file plus names that the
//! resolver actually reached (including imported declarations).

use std::collections::BTreeMap;

use kira_core::Names;
use kira_source::{SourceFile, Span};
use kira_syntax_model::Item;
use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionParams, CompletionTextEdit, Hover as LspHover,
    HoverContents, HoverParams, MarkupContent, MarkupKind, TextEdit,
};

use crate::analysis::{self, Analysis, AnalysisSession};
use crate::convert;
use crate::documents::Documents;

/// Answers hover for a resolved name or declaration.
///
/// The text shown is the source line containing the declaration. It is more
/// useful than only echoing an identifier for Kira's current syntax, where a
/// function signature, a field type, or a local initializer all carry meaning
/// the semantic link intentionally does not duplicate.
pub(crate) fn hover(
    session: &mut AnalysisSession,
    documents: &Documents,
    params: &HoverParams,
) -> Option<LspHover> {
    let uri = &params.text_document_position_params.text_document.uri;
    let text = documents.text(uri)?;
    let analysis = analysis::analyze(session, &analysis_path(uri), text);
    let offset = convert::offset(
        &analysis.file,
        params.text_document_position_params.position,
    );
    let link = link_at(&analysis, offset)?;
    let target = analysis
        .files
        .get(link.definition.source.value() as usize)?;
    let target_line = context_line(target, link.definition.span);
    let target_name = span_text(target, link.definition.span);

    let mut value = match target_line {
        Some(line) => format!("```kira\n{line}\n```").to_owned(),
        None => target_name
            .map(str::to_owned)
            .unwrap_or_else(|| "Kira symbol".to_owned()),
    };
    if link.definition.span.is_empty() {
        value = format!("Kira module `{}`", target.path);
    }
    if target.path != analysis.file.path {
        value.push_str(&format!("\n\nDefined in `{}`", target.path));
    }

    let highlight = if link.reference.source == analysis::DOCUMENT_SOURCE
        && contains(link.reference.span, offset)
    {
        link.reference
    } else {
        link.definition
    };
    let range = (highlight.source == analysis::DOCUMENT_SOURCE && !highlight.span.is_empty())
        .then(|| convert::range(&analysis.file, highlight.span));

    Some(LspHover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value,
        }),
        range,
    })
}

/// Answers completion with deterministic, prefix-filtered symbol items.
///
/// The parser contributes declarations that have not been referenced yet;
/// semantic links contribute locals and imported symbols without needing a
/// second name-resolution implementation. Each completion replaces the word
/// prefix, which keeps acceptance correct in clients that do not perform their
/// own prefix slicing.
pub(crate) fn completion(
    session: &mut AnalysisSession,
    documents: &Documents,
    params: &CompletionParams,
) -> Vec<CompletionItem> {
    let uri = &params.text_document_position.text_document.uri;
    let Some(text) = documents.text(uri) else {
        return Vec::new();
    };
    let analysis = analysis::analyze(session, &analysis_path(uri), text);
    let offset = convert::offset(&analysis.file, params.text_document_position.position);
    let (prefix_start, prefix) = identifier_prefix(text, offset);

    let mut symbols = BTreeMap::new();
    for link in &analysis.definitions {
        let Some(target) = analysis.files.get(link.definition.source.value() as usize) else {
            continue;
        };
        let Some(name) = span_text(target, link.definition.span) else {
            continue;
        };
        add_symbol(
            &mut symbols,
            name,
            symbol_kind(target, link.definition.span),
            context_line(target, link.definition.span),
        );
    }
    collect_declarations(&mut symbols, text);

    let replace = convert::range(
        &analysis.file,
        Span::from_bounds(prefix_start, offset.max(prefix_start)),
    );
    symbols
        .into_iter()
        .filter(|(name, _)| prefix.is_empty() || name.starts_with(prefix))
        .map(|(name, symbol)| CompletionItem {
            label: name.clone(),
            kind: Some(symbol.kind),
            detail: symbol.detail,
            text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                range: replace,
                new_text: name,
            })),
            ..CompletionItem::default()
        })
        .collect()
}

/// A completion candidate, kept private so protocol types do not leak into
/// the source-declaration collector.
struct Symbol {
    kind: CompletionItemKind,
    detail: Option<String>,
}

fn add_symbol(
    symbols: &mut BTreeMap<String, Symbol>,
    name: &str,
    kind: CompletionItemKind,
    detail: Option<String>,
) {
    if is_identifier(name) {
        symbols
            .entry(name.to_owned())
            .or_insert(Symbol { kind, detail });
    }
}

/// Adds names from the current file even when no use has caused a semantic
/// link to be recorded yet. The parser is the same syntax frontend used by the
/// compiler; this is only an index walk, not a competing resolver.
fn collect_declarations(symbols: &mut BTreeMap<String, Symbol>, text: &str) {
    let parsed = kira_parser::parse(analysis::DOCUMENT_SOURCE, text);
    for (source, item) in parsed.tree.items_with_source() {
        if source != analysis::DOCUMENT_SOURCE {
            continue;
        }
        collect_item(symbols, &parsed.interner, item, text);
    }
}

fn collect_item(symbols: &mut BTreeMap<String, Symbol>, names: &Names, item: &Item, text: &str) {
    match item {
        Item::Function(function) => {
            add_declared(
                symbols,
                names,
                function.name,
                function.name_span,
                CompletionItemKind::FUNCTION,
                text,
            );
            for parameter in &function.params {
                add_declared(
                    symbols,
                    names,
                    parameter.name,
                    parameter.name_span,
                    CompletionItemKind::VARIABLE,
                    text,
                );
            }
        }
        Item::Struct(declaration) => {
            add_declared(
                symbols,
                names,
                declaration.name,
                declaration.name_span,
                CompletionItemKind::STRUCT,
                text,
            );
            for field in &declaration.fields {
                add_declared(
                    symbols,
                    names,
                    field.name,
                    field.name_span,
                    CompletionItemKind::FIELD,
                    text,
                );
            }
            for method in &declaration.methods {
                add_function(symbols, names, method, CompletionItemKind::METHOD, text);
            }
        }
        Item::Class(declaration) => {
            add_declared(
                symbols,
                names,
                declaration.name,
                declaration.name_span,
                CompletionItemKind::CLASS,
                text,
            );
            for field in &declaration.fields {
                add_declared(
                    symbols,
                    names,
                    field.name,
                    field.name_span,
                    CompletionItemKind::FIELD,
                    text,
                );
            }
            for field in &declaration.overrides {
                add_declared(
                    symbols,
                    names,
                    field.name,
                    field.name_span,
                    CompletionItemKind::FIELD,
                    text,
                );
            }
            for method in &declaration.methods {
                add_function(
                    symbols,
                    names,
                    &method.function,
                    CompletionItemKind::METHOD,
                    text,
                );
            }
        }
        Item::Enum(declaration) => {
            add_declared(
                symbols,
                names,
                declaration.name,
                declaration.name_span,
                CompletionItemKind::ENUM,
                text,
            );
            for variant in &declaration.variants {
                add_declared(
                    symbols,
                    names,
                    variant.name,
                    variant.name_span,
                    CompletionItemKind::ENUM_MEMBER,
                    text,
                );
            }
        }
        Item::TypeAlias(declaration) => add_declared(
            symbols,
            names,
            declaration.name,
            declaration.name_span,
            CompletionItemKind::TYPE_PARAMETER,
            text,
        ),
        Item::Import(declaration) => {
            if let Some(alias) = declaration.alias
                && let Some(span) = declaration.alias_span
            {
                add_declared(
                    symbols,
                    names,
                    alias,
                    span,
                    CompletionItemKind::MODULE,
                    text,
                );
            }
        }
        Item::Construct(declaration) => {
            add_declared(
                symbols,
                names,
                declaration.name,
                declaration.name_span,
                CompletionItemKind::CLASS,
                text,
            );
            for field in &declaration.fields {
                add_declared(
                    symbols,
                    names,
                    field.name,
                    field.name_span,
                    CompletionItemKind::FIELD,
                    text,
                );
            }
            for method in &declaration.methods {
                add_function(
                    symbols,
                    names,
                    &method.function,
                    CompletionItemKind::METHOD,
                    text,
                );
            }
        }
        Item::Extend(declaration) => {
            for method in &declaration.methods {
                add_function(symbols, names, method, CompletionItemKind::METHOD, text);
            }
        }
        Item::Trait(declaration) => {
            add_declared(
                symbols,
                names,
                declaration.name,
                declaration.name_span,
                CompletionItemKind::INTERFACE,
                text,
            );
            for member in &declaration.members {
                add_function(
                    symbols,
                    names,
                    &member.function,
                    CompletionItemKind::METHOD,
                    text,
                );
            }
        }
        Item::Unsupported(_) => {}
    }
}

fn add_function(
    symbols: &mut BTreeMap<String, Symbol>,
    names: &Names,
    function: &kira_syntax_model::Function,
    kind: CompletionItemKind,
    text: &str,
) {
    add_declared(
        symbols,
        names,
        function.name,
        function.name_span,
        kind,
        text,
    );
    for parameter in &function.params {
        add_declared(
            symbols,
            names,
            parameter.name,
            parameter.name_span,
            CompletionItemKind::VARIABLE,
            text,
        );
    }
}

fn add_declared(
    symbols: &mut BTreeMap<String, Symbol>,
    names: &Names,
    symbol: kira_core::Symbol,
    span: Span,
    kind: CompletionItemKind,
    text: &str,
) {
    add_symbol(
        symbols,
        names.resolve(symbol),
        kind,
        context_line_text(text, span),
    );
}

fn link_at(analysis: &Analysis, offset: u32) -> Option<kira_semantics::DefinitionLink> {
    analysis
        .definitions
        .iter()
        .copied()
        .filter(|link| {
            (link.reference.source == analysis::DOCUMENT_SOURCE
                && contains(link.reference.span, offset))
                || (link.definition.source == analysis::DOCUMENT_SOURCE
                    && contains(link.definition.span, offset))
        })
        .min_by_key(|link| {
            let reference = if link.reference.source == analysis::DOCUMENT_SOURCE {
                link.reference.span.len
            } else {
                u32::MAX
            };
            let definition = if link.definition.source == analysis::DOCUMENT_SOURCE {
                link.definition.span.len
            } else {
                u32::MAX
            };
            reference.min(definition)
        })
}

fn analysis_path(uri: &lsp_types::Uri) -> String {
    crate::documents::file_path(uri).unwrap_or_else(|| crate::documents::display_name(uri))
}

fn contains(span: Span, offset: u32) -> bool {
    span.start <= offset && offset <= span.end()
}

fn span_text(file: &SourceFile, span: Span) -> Option<&str> {
    let start = span.start as usize;
    let end = span.end() as usize;
    file.text.get(start..end)
}

fn context_line(file: &SourceFile, span: Span) -> Option<String> {
    if file.text.is_empty() {
        return None;
    }
    let offset = span.start.min(file.text.len() as u32);
    let line = file.line_map.line_index(offset);
    let (start, end) = file.line_map.line_bounds(line, &file.text);
    short_line(file.text.get(start as usize..end as usize)?)
}

fn context_line_text(text: &str, span: Span) -> Option<String> {
    if text.is_empty() {
        return None;
    }
    let offset = span.start.min(text.len() as u32) as usize;
    let before = text.get(..offset)?;
    let start = before.rfind('\n').map_or(0, |index| index + 1);
    let rest = text.get(offset..)?;
    let end = rest.find('\n').map_or(text.len(), |index| offset + index);
    short_line(text.get(start..end)?)
}

fn short_line(line: &str) -> Option<String> {
    let line = line.trim();
    if line.is_empty() {
        None
    } else {
        Some(line.chars().take(200).collect())
    }
}

fn symbol_kind(file: &SourceFile, span: Span) -> CompletionItemKind {
    let line = context_line(file, span).unwrap_or_default();
    let before = line
        .split_once(span_text(file, span).unwrap_or_default())
        .map(|(before, _)| before.trim_start())
        .unwrap_or_default();
    if before.starts_with("function") || before.starts_with("async function") {
        CompletionItemKind::FUNCTION
    } else if before.starts_with("struct") {
        CompletionItemKind::STRUCT
    } else if before.starts_with("class") {
        CompletionItemKind::CLASS
    } else if before.starts_with("enum") {
        CompletionItemKind::ENUM
    } else if before.starts_with("trait") {
        CompletionItemKind::INTERFACE
    } else if before.starts_with("type") {
        CompletionItemKind::TYPE_PARAMETER
    } else if before.starts_with("let") || before.starts_with("var") {
        CompletionItemKind::VARIABLE
    } else {
        CompletionItemKind::FIELD
    }
}

fn identifier_prefix(text: &str, offset: u32) -> (u32, &str) {
    let end = offset.min(text.len() as u32) as usize;
    let mut start = end;
    while start > 0 {
        let Some((candidate, character)) = text[..start].char_indices().next_back() else {
            break;
        };
        if !is_identifier_char(character) {
            break;
        }
        start = candidate;
    }
    (start as u32, &text[start..end])
}

fn is_identifier(name: &str) -> bool {
    let mut characters = name.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    (first == '_' || first.is_alphabetic()) && characters.all(is_identifier_char)
}

fn is_identifier_char(character: char) -> bool {
    character == '_' || character.is_alphanumeric()
}
