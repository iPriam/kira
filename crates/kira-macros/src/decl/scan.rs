//! The locating scan itself: where each declaration, member, field and
//! annotation begins and ends.
//!
//! These answer in token indices and byte ranges and never build a node, so a
//! caller can rewrite one span and leave every other byte of the file alone.

use kira_source::Span;
use kira_syntax_model::TokenKind;

use super::model::{Annotation, Declaration, DeclarationKind, Field, Hook, Member};
use crate::tokens::Lexed;

/// Whether `kind` can only be the first token of a declaration, so that
/// reaching it while scanning a bodyless one means the latter has ended.
pub(super) fn starts_declaration(kind: TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::At
            | TokenKind::Struct
            | TokenKind::Class
            | TokenKind::Enum
            | TokenKind::Construct
            | TokenKind::Function
            | TokenKind::Distinct
            | TokenKind::Trait
            | TokenKind::Import
            | TokenKind::Type
    )
}

/// Scans `distinct Name = Representation`, which ends at its representation.
///
/// The one readable member a distinct type has is reported as a field called
/// `raw` whose written type is the representation, because that is exactly what
/// `id.raw` reads. A derive that walks `target.fields` therefore folds, compares,
/// and serializes a distinct type through the same one loop it uses for a
/// struct, and the only thing it needs its own branch for is *construction*,
/// which is `Name(value)` rather than a brace literal.
pub(super) fn scan_distinct(
    file: &Lexed<'_>,
    head: usize,
    name_index: usize,
    name: String,
    annotations: Vec<Annotation>,
) -> Option<(Declaration, usize)> {
    if file.kind(name_index + 1) != TokenKind::Equals {
        return None;
    }
    let representation = name_index + 2;
    if !file.is_ident(representation) {
        return None;
    }
    // A qualified representation — `Foundation.Byte` — is one type written in
    // three tokens, so the scan keeps taking `. name` pairs.
    let mut last = representation;
    while file.kind(last + 1) == TokenKind::Dot && file.is_ident(last + 2) {
        last += 2;
    }
    let representation_span = file.span_of(representation, last);
    let span = file.span_of(head, last);
    let raw = Field {
        name: "raw".to_owned(),
        type_text: file.slice(representation_span).to_owned(),
        initializer: String::new(),
        syntax: file.slice(representation_span).to_owned(),
        span: representation_span,
        source: Some(file.source),
        annotations: Vec::new(),
    };
    Some((
        Declaration {
            kind: DeclarationKind::Distinct,
            name,
            family: String::new(),
            fields: vec![raw],
            members: Vec::new(),
            hooks: Vec::new(),
            syntax: file.slice(span).to_owned(),
            span,
            source: Some(file.source),
            path: file.path.clone(),
            line: file.line_of(span.start),
            file_lines: file.line_count(),
            annotations,
        },
        last + 1,
    ))
}

/// Consumes a run of `@Name` / `@Derive(A, B)` annotations.
pub(super) fn scan_annotations(file: &Lexed<'_>, start: usize) -> (Vec<Annotation>, usize) {
    let mut annotations = Vec::new();
    let mut index = start;
    while file.kind(index) == TokenKind::At {
        if !file.is_ident(index + 1) {
            break;
        }
        let mut name = file.text_at(index + 1).to_owned();
        let mut last = index + 1;
        // A qualified annotation — `@FFI.Extern` — keeps its dotted spelling so
        // it is never mistaken for a macro named after its first segment.
        while file.kind(last + 1) == TokenKind::Dot && file.is_ident(last + 2) {
            name.push('.');
            name.push_str(file.text_at(last + 2));
            last += 2;
        }
        let mut arguments = Vec::new();
        if file.kind(last + 1) == TokenKind::LParen {
            if let Some(close) = file.match_close(last + 1) {
                arguments = file
                    .split_group(last + 1, close)
                    .into_iter()
                    .map(|(first, end)| file.slice(file.span_of(first, end)).trim().to_owned())
                    .collect();
                last = close;
            }
        } else if file.kind(last + 1) == TokenKind::LBrace {
            // `@FFI.Extern { … }` and `@Export { … }` carry a block payload.
            if let Some(close) = file.match_close(last + 1) {
                last = close;
            }
        }
        annotations.push(Annotation {
            name,
            arguments,
            span: file.span_of(index, last),
        });
        index = last + 1;
    }
    (annotations, index)
}

/// The family named by the `extends` clause of the declaration whose name sits
/// at `name_index`.
///
/// Empty when the header names none, which is a declaration the parser has
/// already refused; the scan answers rather than failing, so a macro run over a
/// file with one mistake in it still sees every other declaration.
pub(super) fn backing_family(file: &Lexed<'_>, name_index: usize) -> String {
    let mut index = name_index + 1;
    while !matches!(file.kind(index), TokenKind::LBrace | TokenKind::Eof) {
        if file.kind(index) == TokenKind::Extends && file.is_ident(index + 1) {
            return file.text_at(index + 1).to_owned();
        }
        index += 1;
    }
    String::new()
}

/// Scans a construct family's `lifecycle { … }` section, if it has one.
///
/// A hook is `name() { … }`, optionally annotated. `lifecycle` is a contextual
/// identifier: a member of that name followed by a brace is the section, and
/// anything else called `lifecycle` is left alone.
pub(super) fn scan_hooks(file: &Lexed<'_>, open: usize, close: usize) -> Vec<Hook> {
    let mut index = open + 1;
    while index < close {
        if file.is_word(index, "lifecycle") && file.kind(index + 1) == TokenKind::LBrace {
            let Some(section_close) = file.match_close(index + 1) else {
                return Vec::new();
            };
            return scan_hook_bodies(file, index + 1, section_close);
        }
        if matches!(
            file.kind(index),
            TokenKind::LBrace | TokenKind::LParen | TokenKind::LBracket
        ) {
            match file.match_close(index) {
                Some(end) => index = end + 1,
                None => return Vec::new(),
            }
            continue;
        }
        index += 1;
    }
    Vec::new()
}

/// Scans the hooks inside a `lifecycle { … }` section.
pub(super) fn scan_hook_bodies(file: &Lexed<'_>, open: usize, close: usize) -> Vec<Hook> {
    let mut hooks = Vec::new();
    let mut index = open + 1;
    while index < close {
        let (annotations, after) = scan_annotations(file, index);
        if !file.is_ident(after) || file.kind(after + 1) != TokenKind::LParen {
            index = if after > index { after } else { index + 1 };
            continue;
        }
        let Some(arguments_close) = file.match_close(after + 1) else {
            break;
        };
        let brace = arguments_close + 1;
        if file.kind(brace) != TokenKind::LBrace {
            index = brace;
            continue;
        }
        let Some(end) = file.match_close(brace) else {
            break;
        };
        hooks.push(Hook {
            name: file.text_at(after).to_owned(),
            body: body_text(file, brace, end),
            comptime: annotations
                .iter()
                .any(|annotation| annotation.name == "Comptime"),
            span: file.span_of(index, end),
        });
        index = end + 1;
    }
    hooks
}

/// Scans the behaviour members of a declaration body.
///
/// Both spellings a construct-backed declaration may use: the `name { … }`
/// shorthand, and `function name(…) -> T { … }`. A `let` member is a field and
/// is scanned by [`scan_fields`] instead.
pub(super) fn scan_members(file: &Lexed<'_>, open: usize, close: usize) -> Vec<Member> {
    let mut members = Vec::new();
    let mut index = open + 1;
    while index < close {
        // `function name(…) … { … }`. A `@Required` member has no body at all —
        // it states an obligation — so the search for one stops at the next
        // member rather than running on and swallowing that member's braces.
        if file.kind(index) == TokenKind::Function && file.is_ident(index + 1) {
            let name = file.text_at(index + 1).to_owned();
            let mut brace = index + 2;
            while brace < close && file.kind(brace) != TokenKind::LBrace {
                if matches!(
                    file.kind(brace),
                    TokenKind::Function | TokenKind::At | TokenKind::Let | TokenKind::Var
                ) {
                    break;
                }
                // A parameter list or a `[T]` is skipped whole: neither holds
                // the body, and a `(` here would otherwise be walked into.
                if matches!(file.kind(brace), TokenKind::LParen | TokenKind::LBracket) {
                    match file.match_close(brace) {
                        Some(end) => brace = end + 1,
                        None => break,
                    }
                    continue;
                }
                brace += 1;
            }
            if file.kind(brace) != TokenKind::LBrace {
                // A requirement, not an implementation. Nothing to run.
                index = brace.max(index + 1);
                continue;
            }
            let Some(end) = file.match_close(brace) else {
                break;
            };
            members.push(Member {
                name,
                body: body_text(file, brace, end),
            });
            index = end + 1;
            continue;
        }
        if matches!(
            file.kind(index),
            TokenKind::At | TokenKind::Let | TokenKind::Var
        ) {
            let (_, after) = scan_annotations(file, index);
            if matches!(file.kind(after), TokenKind::Let | TokenKind::Var)
                && file.is_ident(after + 1)
            {
                let end = member_end(file, after + 2, close);
                index = end.saturating_add(1);
                continue;
            }
        }
        // `name { … }`, the shorthand for the member the family calls `name`.
        if file.is_ident(index) && file.kind(index + 1) == TokenKind::LBrace {
            let name = file.text_at(index).to_owned();
            let Some(end) = file.match_close(index + 1) else {
                break;
            };
            members.push(Member {
                name,
                body: body_text(file, index + 1, end),
            });
            index = end + 1;
            continue;
        }
        index += 1;
    }
    members
}

/// The text between a body's braces.
pub(super) fn body_text(file: &Lexed<'_>, open: usize, close: usize) -> String {
    file.slice(Span::from_bounds(
        file.span(open).end(),
        file.span(close).start,
    ))
    .to_owned()
}

/// Scans the `var` / `let` members of a declaration body.
pub(super) fn scan_fields(file: &Lexed<'_>, open: usize, close: usize) -> Vec<Field> {
    let mut fields = Vec::new();
    let mut index = open + 1;
    while index < close {
        if file.kind(index) == TokenKind::Function {
            index = skip_member(file, index, close);
            continue;
        }
        if is_named_rule_start(file, index) {
            index = file
                .match_close(index + 1)
                .map_or(close, |end| end.saturating_add(1));
            continue;
        }
        if file.kind(index) != TokenKind::At
            && !matches!(file.kind(index), TokenKind::Let | TokenKind::Var)
        {
            index += 1;
            continue;
        }
        let (annotations, after) = scan_annotations(file, index);
        if !matches!(file.kind(after), TokenKind::Let | TokenKind::Var) || !file.is_ident(after + 1)
        {
            index = if after > index { after } else { index + 1 };
            continue;
        }
        let name = file.text_at(after + 1).to_owned();
        let end = member_end(file, after + 2, close);
        let (type_text, initializer) = split_annotation(file, after + 2, end);
        let span = file.span_of(index, end);
        fields.push(Field {
            name,
            type_text,
            initializer,
            syntax: file.slice(span).to_owned(),
            span,
            source: Some(file.source),
            annotations,
        });
        index = end + 1;
    }
    fields
}

/// Scans an enum body's variants.
///
/// A variant surfaces as a field: its name is the variant's, and its type is
/// the payload type or the empty string, which is what lets one derive macro
/// walk a struct and an enum with the same loop.
pub(super) fn scan_variants(file: &Lexed<'_>, open: usize, close: usize) -> Vec<Field> {
    let mut fields = Vec::new();
    let mut index = open + 1;
    while index < close {
        if !file.is_ident(index) {
            index += 1;
            continue;
        }
        let name = file.text_at(index).to_owned();
        let mut last = index;
        let mut type_text = String::new();
        if file.kind(index + 1) == TokenKind::Colon {
            let end = payload_end(file, index + 2, close);
            type_text = file.slice(file.span_of(index + 2, end)).trim().to_owned();
            last = end;
        } else if file.kind(index + 1) == TokenKind::LParen {
            // `Rank(Int)` — the payload form with no default. Scanning only the
            // `Name: Type` form left the parenthesized payload unread AND the
            // scan positioned inside the parentheses, so the payload's TYPE was
            // then reflected as a variant of its own: `Note { Rank(Int) }` came
            // back as two variants, `Rank` and `Int`, and every derive built
            // over `target.fields` emitted an arm for a variant that does not
            // exist.
            if let Some(end) = file.match_close(index + 1) {
                if end > index + 2 {
                    type_text = file
                        .slice(file.span_of(index + 2, end - 1))
                        .trim()
                        .to_owned();
                }
                last = end;
            }
        }
        let span = file.span_of(index, last);
        fields.push(Field {
            name,
            type_text,
            initializer: String::new(),
            syntax: file.slice(span).to_owned(),
            span,
            source: Some(file.source),
            annotations: Vec::new(),
        });
        index = last + 1;
    }
    fields
}

/// The last token index of an enum variant's payload type, which starts at
/// `from`.
///
/// Variants have no leading keyword to end on, so the payload runs to the end
/// of its own line: `Ok: Int` on one line and `Error: AppError` on the next are
/// two variants, not one with a very long type.
pub(super) fn payload_end(file: &Lexed<'_>, from: usize, close: usize) -> usize {
    let mut last = from;
    let mut index = from + 1;
    while index < close && !file.newline_before(index) && file.kind(index) != TokenKind::Comma {
        match file.kind(index) {
            TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => {
                match file.match_close(index) {
                    Some(end) => {
                        last = end;
                        index = end + 1;
                        continue;
                    }
                    None => break,
                }
            }
            _ => last = index,
        }
        index += 1;
    }
    last
}

/// The last token index of the member that starts at `from`.
///
/// A member ends where the next one begins: at the next annotation, `let`,
/// `var`, or `function` at the body's own depth, or at the closing brace.
pub(super) fn member_end(file: &Lexed<'_>, from: usize, close: usize) -> usize {
    let mut index = from;
    let mut last = from;
    let mut saw_equals = false;
    let mut saw_value = false;
    while index < close {
        if is_named_rule_start(file, index) && (!saw_equals || saw_value) {
            break;
        }
        match file.kind(index) {
            TokenKind::At | TokenKind::Let | TokenKind::Var | TokenKind::Function => break,
            TokenKind::Equals => {
                saw_equals = true;
                last = index;
            }
            TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => {
                match file.match_close(index) {
                    Some(end) => {
                        last = end;
                        if saw_equals {
                            saw_value = true;
                        }
                        index = end + 1;
                        continue;
                    }
                    None => break,
                }
            }
            _ => {
                if saw_equals {
                    saw_value = true;
                }
                last = index;
            }
        }
        index += 1;
    }
    last.max(from.saturating_sub(1))
}

/// Splits `: Type = initializer` into its two written halves.
pub(super) fn split_annotation(file: &Lexed<'_>, from: usize, end: usize) -> (String, String) {
    let mut index = from;
    if file.kind(index) == TokenKind::Colon {
        index += 1;
    }
    let type_start = index;
    let mut equals = None;
    while index <= end {
        match file.kind(index) {
            TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => {
                match file.match_close(index) {
                    Some(close) => index = close,
                    None => break,
                }
            }
            TokenKind::Equals => {
                equals = Some(index);
                break;
            }
            _ => {}
        }
        index += 1;
    }
    match equals {
        Some(at) if at > type_start => (
            file.slice(file.span_of(type_start, at - 1))
                .trim()
                .to_owned(),
            file.slice(file.span_of(at + 1, end)).trim().to_owned(),
        ),
        // `let x = 1`, with no written type: the `=` is the first token, so
        // there is no type half and the initializer starts *after* it. Slicing
        // from `from` here would hand back `= 1` and call it the initializer,
        // which is what a reader comparing against `1` would never match.
        Some(at) => (
            String::new(),
            file.slice(file.span_of(at + 1, end)).trim().to_owned(),
        ),
        None if end >= type_start => (
            file.slice(file.span_of(type_start, end)).trim().to_owned(),
            String::new(),
        ),
        None => (String::new(), String::new()),
    }
}

/// Skips a member with a `{ … }` body, returning the index just past it.
pub(super) fn skip_member(file: &Lexed<'_>, from: usize, close: usize) -> usize {
    let mut index = from;
    while index < close {
        if file.kind(index) == TokenKind::LBrace {
            return file.match_close(index).map_or(close, |end| end + 1);
        }
        index += 1;
    }
    close
}

pub(super) fn is_named_rule_start(file: &Lexed<'_>, index: usize) -> bool {
    file.is_ident(index) && file.kind(index + 1) == TokenKind::LBrace
}
