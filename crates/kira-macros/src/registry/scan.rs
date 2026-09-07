//! Reading one file's macro declarations off its token stream.
//!
//! Declarations are located at brace depth 0: `macro` and `comptime` are
//! contextual identifiers, so a local called `macro` inside a function body
//! is never mistaken for one. Total, like the rest of expansion — a malformed
//! declaration is reported and dropped, and the file is still read to its end.

use kira_source::Span;
use kira_syntax_model::TokenKind;

use crate::diagnostics::{self, Reporter};
use crate::tokens::Lexed;

use super::FileRegistry;
use super::model::{
    ComptimeFunction, Declarative, Fragment, FragmentKind, Procedural, ProceduralKind, kind_word,
};

/// Collects every macro declaration in one file, reporting malformed ones.
pub(crate) fn collect_file(file: &Lexed<'_>, reporter: &mut Reporter) -> FileRegistry {
    let mut found = FileRegistry::default();
    let mut index = 0usize;
    while index < file.len() {
        match file.kind(index) {
            TokenKind::Eof => break,
            TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => {
                match file.match_close(index) {
                    Some(end) => index = end + 1,
                    None => break,
                }
                continue;
            }
            _ => {}
        }
        // `enum Name { A B }` — read for its case names, so a macro body may
        // name one. Only the shape is taken; what the cases mean is the
        // program's business. A generic `enum Result<Value, Failure> { … }`
        // carries its parameter list between the name and the body, so the
        // body is found by walking that list rather than by assuming it is
        // exactly two tokens away. The walk is bounded to what a parameter
        // list can be — identifiers, commas, and angle brackets — so a
        // malformed enum never swallows some later declaration's brace.
        if file.kind(index) == TokenKind::Enum && file.is_ident(index + 1) {
            let mut probe = index + 2;
            let mut angle_depth = 0u32;
            let mut body = None;
            while probe < file.len() && probe <= index + 16 {
                match file.kind(probe) {
                    TokenKind::LBrace => {
                        body = Some(probe);
                        break;
                    }
                    TokenKind::Lt => angle_depth += 1,
                    TokenKind::Gt => angle_depth = angle_depth.saturating_sub(1),
                    TokenKind::Identifier | TokenKind::Comma if angle_depth > 0 => {}
                    _ => break,
                }
                probe += 1;
            }
            let Some(brace) = body else {
                index += 1;
                continue;
            };
            let Some(end) = file.match_close(brace) else {
                break;
            };
            let name = file.text_at(index + 1).to_owned();
            let mut variants = Vec::new();
            let mut at = brace + 1;
            while at < end {
                // A case is an identifier at the top of the body; anything
                // nested belongs to a payload and is skipped whole.
                match file.kind(at) {
                    TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => {
                        match file.match_close(at) {
                            Some(close) => at = close + 1,
                            None => break,
                        }
                        continue;
                    }
                    _ => {}
                }
                if file.is_ident(at) {
                    variants.push(file.text_at(at).to_owned());
                }
                at += 1;
            }
            found.enums.push((name, variants));
            index = end + 1;
            continue;
        }
        // `function` is a keyword token, not a contextual identifier the way
        // `macro` is, so it is matched by kind rather than by text.
        if file.is_word(index, "comptime") && file.kind(index + 1) == TokenKind::Function {
            match scan_comptime_function(file, index, reporter) {
                Some((function, span, next)) => {
                    found.spans.push(span);
                    found.comptime_functions.push(function);
                    index = next;
                    continue;
                }
                // The definition's structure is unknowable past this point, so
                // parsing the tail would report the raw `quote` text as broken
                // Kira rather than anything the author wrote. Blank it: the
                // scan error above already names the failure.
                None => {
                    blank_to_end(file, index, &mut found.spans);
                    break;
                }
            }
        }
        if file.is_word(index, "comptime") && file.is_word(index + 1, "macro") {
            match scan_procedural(file, index, reporter) {
                Some((macro_declaration, span, next)) => {
                    found.spans.push(span);
                    found.procedural.push(macro_declaration);
                    index = next;
                    continue;
                }
                None => {
                    blank_to_end(file, index, &mut found.spans);
                    break;
                }
            }
        }
        if file.is_word(index, "macro")
            && file.is_ident(index + 1)
            && file.kind(index + 2) == TokenKind::LParen
        {
            match scan_declarative(file, index, reporter) {
                Some((macro_declaration, span, next)) => {
                    found.spans.push(span);
                    found.declarative.push(macro_declaration);
                    index = next;
                    continue;
                }
                None => {
                    blank_to_end(file, index, &mut found.spans);
                    break;
                }
            }
        }
        index += 1;
    }
    found
}

/// Blanks the file from `index` to its end, so a definition whose structure
/// the scanner could not recover never reaches the parser as raw text.
///
/// Without this, an unclosed macro body leaves every later `#{` in place and
/// each one is reported as an unexpected character, burying the scan error
/// that names the actual failure under noise from text the author never meant
/// as code.
fn blank_to_end(file: &Lexed<'_>, index: usize, spans: &mut Vec<Span>) {
    let start = file.span(index).start;
    let end = file.text.len() as u32;
    if start < end {
        spans.push(Span::from_bounds(start, end));
    }
}

/// Scans `macro Name(p: expr) { expand { … } }` starting at the `macro` word.
fn scan_declarative(
    file: &Lexed<'_>,
    start: usize,
    reporter: &mut Reporter,
) -> Option<(Declarative, Span, usize)> {
    let name = file.text_at(start + 1).to_owned();
    let name_span = file.span(start + 1);
    let open_params = start + 2;
    let Some(close_params) = file.match_close(open_params) else {
        // Reported rather than silent: a `None` here would otherwise stop the
        // whole file's collection with no diagnostic, and every macro declared
        // after this one would be reported as unknown at its call sites.
        reporter.error(
            file.source,
            file.span(open_params),
            diagnostics::EXPAND_SIGNATURE,
            format!("macro `{name}` has an unclosed `( … )` parameter list; the rest of this file was skipped"),
        );
        return None;
    };
    let mut fragments = Vec::new();
    for (first, last) in file.split_group(open_params, close_params) {
        let parameter_name = file.text_at(first).to_owned();
        let kind = if file.kind(first + 1) == TokenKind::Colon {
            match file.text_at(first + 2) {
                "expr" => Some(FragmentKind::Expr),
                "place" => Some(FragmentKind::Place),
                _ => None,
            }
        } else {
            None
        };
        match kind {
            Some(kind) => fragments.push(Fragment {
                name: parameter_name,
                kind,
            }),
            None => reporter.error(
                file.source,
                file.span_of(first, last),
                diagnostics::FRAGMENT_KIND,
                format!(
                    "macro `{name}` parameter `{parameter_name}` must declare a fragment kind; \
                     v1 has `expr` and `place`"
                ),
            ),
        }
    }

    let open_body = close_params + 1;
    if file.kind(open_body) != TokenKind::LBrace {
        reporter.error(
            file.source,
            file.span(open_body),
            diagnostics::EXPAND_SIGNATURE,
            format!("macro `{name}` needs a body containing `expand {{ … }}`"),
        );
        return None;
    }
    let Some(close_body) = file.match_close(open_body) else {
        reporter.error(
            file.source,
            file.span(open_body),
            diagnostics::EXPAND_SIGNATURE,
            format!(
                "macro `{name}` has an unclosed `{{ … }}` body; the rest of this file was skipped"
            ),
        );
        return None;
    };
    let template = match find_expand_block(file, open_body, close_body) {
        Some((open, close)) => file
            .slice(Span::from_bounds(
                file.span(open).end(),
                file.span(close).start,
            ))
            .to_owned(),
        None => {
            reporter.error(
                file.source,
                file.span_of(start, open_body),
                diagnostics::EXPAND_SIGNATURE,
                format!("macro `{name}` has no `expand {{ … }}` template"),
            );
            return None;
        }
    };

    Some((
        Declarative {
            name,
            fragments,
            template,
            source: file.source,
            span: name_span,
        },
        file.span_of(start, close_body),
        close_body + 1,
    ))
}

/// The `expand { … }` braces inside a declarative macro's body.
fn find_expand_block(file: &Lexed<'_>, open: usize, close: usize) -> Option<(usize, usize)> {
    let mut index = open + 1;
    while index < close {
        if file.is_word(index, "expand") && file.kind(index + 1) == TokenKind::LBrace {
            let block_close = file.match_close(index + 1)?;
            return Some((index + 1, block_close));
        }
        index += 1;
    }
    None
}

/// Scans `comptime function name(…) -> T { … }` starting at the `comptime` word.
///
/// The shape is an ordinary function declaration, so only the name, the
/// parameter names and the body text are taken: the written types are the
/// analyzer's business, and the evaluator binds arguments by position.
fn scan_comptime_function(
    file: &Lexed<'_>,
    start: usize,
    reporter: &mut Reporter,
) -> Option<(ComptimeFunction, Span, usize)> {
    let name_index = start + 2;
    if !file.is_ident(name_index) {
        reporter.error(
            file.source,
            file.span(name_index),
            diagnostics::BAD_KIND,
            "expected a name after `comptime function`",
        );
        return None;
    }
    let name = file.text_at(name_index).to_owned();
    let name_span = file.span(name_index);
    let open_parameters = name_index + 1;
    if file.kind(open_parameters) != TokenKind::LParen {
        reporter.error(
            file.source,
            name_span,
            diagnostics::EXPAND_SIGNATURE,
            format!("`comptime function {name}` needs a parameter list"),
        );
        return None;
    }
    let Some(close_parameters) = file.match_close(open_parameters) else {
        reporter.error(
            file.source,
            name_span,
            diagnostics::EXPAND_SIGNATURE,
            format!("`comptime function {name}` has an unclosed `( … )` parameter list; the rest of this file was skipped"),
        );
        return None;
    };
    let parameters = file
        .split_group(open_parameters, close_parameters)
        .into_iter()
        .map(|(first, _)| file.text_at(first).to_owned())
        .collect();
    // Whatever sits between the parameters and the body is the written result
    // type, which the evaluator does not need: the value it produces carries its
    // own shape, and the analyzer checks the call site against the declaration.
    let mut open_body = close_parameters + 1;
    while open_body < file.len() && file.kind(open_body) != TokenKind::LBrace {
        if file.kind(open_body) == TokenKind::Eof {
            reporter.error(
                file.source,
                name_span,
                diagnostics::EXPAND_SIGNATURE,
                format!("`comptime function {name}` needs a `{{ … }}` body"),
            );
            return None;
        }
        open_body += 1;
    }
    let Some(close_body) = file.match_close(open_body) else {
        reporter.error(
            file.source,
            name_span,
            diagnostics::EXPAND_SIGNATURE,
            format!("`comptime function {name}` has an unclosed `{{ … }}` body; the rest of this file was skipped"),
        );
        return None;
    };
    let body_span = Span::from_bounds(file.span(open_body).end(), file.span(close_body).start);
    let body = file.slice(body_span).to_owned();
    Some((
        ComptimeFunction {
            name,
            parameters,
            body,
            body_span,
            source: file.source,
            span: name_span,
        },
        file.span_of(start, close_body),
        close_body + 1,
    ))
}

/// Scans `comptime macro Name { … }` starting at the `comptime` word.
fn scan_procedural(
    file: &Lexed<'_>,
    start: usize,
    reporter: &mut Reporter,
) -> Option<(Procedural, Span, usize)> {
    let name_index = start + 2;
    if !file.is_ident(name_index) {
        reporter.error(
            file.source,
            file.span(name_index),
            diagnostics::BAD_KIND,
            "expected a name after `comptime macro`",
        );
        return None;
    }
    let name = file.text_at(name_index).to_owned();
    let open_body = name_index + 1;
    if file.kind(open_body) != TokenKind::LBrace {
        reporter.error(
            file.source,
            file.span(open_body),
            diagnostics::BAD_KIND,
            format!("`comptime macro {name}` needs a `{{ … }}` body"),
        );
        return None;
    }
    let Some(close_body) = file.match_close(open_body) else {
        reporter.error(
            file.source,
            file.span(open_body),
            diagnostics::EXPAND_SIGNATURE,
            format!("`comptime macro {name}` has an unclosed `{{ … }}` body; the rest of this file was skipped"),
        );
        return None;
    };

    let mut kind = None;
    let mut applies_to = Vec::new();
    let mut trigger_field = false;
    let mut replace = false;
    let mut parameters = Vec::new();
    let mut body = None;

    let mut index = open_body + 1;
    while index < close_body {
        if file.is_word(index, "expand") && file.kind(index + 1) == TokenKind::LParen {
            let Some(close_parameters) = file.match_close(index + 1) else {
                reporter.error(
                    file.source,
                    file.span(index + 1),
                    diagnostics::EXPAND_SIGNATURE,
                    format!(
                        "`comptime macro {name}` has an unclosed `expand( … )` parameter list; \
                         the rest of this file was skipped"
                    ),
                );
                return None;
            };
            parameters = file
                .split_group(index + 1, close_parameters)
                .into_iter()
                .map(|(first, _)| file.text_at(first).to_owned())
                .collect();
            let mut brace = close_parameters + 1;
            while brace < close_body && file.kind(brace) != TokenKind::LBrace {
                brace += 1;
            }
            let Some(close_expand) = file.match_close(brace) else {
                reporter.error(
                    file.source,
                    file.span(brace.min(close_body)),
                    diagnostics::EXPAND_SIGNATURE,
                    format!(
                        "`comptime macro {name}` has an unclosed `expand {{ … }}` body; the \
                         rest of this file was skipped"
                    ),
                );
                return None;
            };
            let body_span =
                Span::from_bounds(file.span(brace).end(), file.span(close_expand).start);
            body = Some((file.slice(body_span).to_owned(), body_span));
            index = close_expand + 1;
            continue;
        }
        if file.is_ident(index) && file.kind(index + 1) == TokenKind::LBrace {
            let member = file.text_at(index);
            let Some(close_member) = file.match_close(index + 1) else {
                reporter.error(
                    file.source,
                    file.span(index + 1),
                    diagnostics::BAD_KIND,
                    format!(
                        "`comptime macro {name}` has an unclosed `{member} {{ … }}`; the rest \
                         of this file was skipped"
                    ),
                );
                return None;
            };
            // `function`, `struct`, `enum`, `class`, and `true` are all real
            // keywords, so a member's words are read by *text* rather than by
            // token kind: `kind { function }` names a macro form, not a
            // declaration.
            let words: Vec<&str> = (index + 2..close_member)
                .filter(|&word| file.kind(word) != TokenKind::Comma)
                .map(|word| file.text_at(word))
                .collect();
            match member {
                "kind" => {
                    kind = words
                        .first()
                        .and_then(|word| ProceduralKind::from_word(word))
                }
                "appliesTo" => {
                    applies_to = words.iter().map(|word| (*word).to_owned()).collect();
                }
                "trigger" => trigger_field = words.contains(&"field"),
                "replace" => replace = words.contains(&"true"),
                _ => {}
            }
            index = close_member + 1;
            continue;
        }
        index += 1;
    }

    let declaration_span = file.span_of(start, close_body);
    let name_span = file.span(name_index);
    let Some(kind) = kind else {
        reporter.error(
            file.source,
            name_span,
            diagnostics::BAD_KIND,
            format!(
                "`comptime macro {name}` needs `kind {{ function | attribute | derive | wrapper }}`"
            ),
        );
        return None;
    };
    let Some((body, body_span)) = body else {
        reporter.error(
            file.source,
            name_span,
            diagnostics::EXPAND_SIGNATURE,
            format!("`comptime macro {name}` must define `expand`"),
        );
        return None;
    };

    let declared = Procedural {
        name,
        kind,
        applies_to,
        trigger_field,
        replace,
        parameters,
        body,
        body_span,
        source: file.source,
        span: name_span,
    };
    validate_shape(&declared, reporter);
    Some((declared, declaration_span, close_body + 1))
}

/// Checks the members of a `comptime macro` against each other.
fn validate_shape(declared: &Procedural, reporter: &mut Reporter) {
    let Procedural {
        name,
        kind,
        applies_to,
        trigger_field,
        replace,
        parameters,
        source,
        span,
        ..
    } = declared;
    let (source, span, kind) = (*source, *span, *kind);
    match kind {
        ProceduralKind::Collector if !applies_to.is_empty() => reporter.error(
            source,
            span,
            diagnostics::APPLIES_TO_PRESENCE,
            format!("`appliesTo` says which declarations a macro may annotate, so a `collector` macro like `{name}`, which annotates none, has none"),
        ),
        ProceduralKind::Function if !applies_to.is_empty() => reporter.error(
            source,
            span,
            diagnostics::APPLIES_TO_PRESENCE,
            format!("`appliesTo` says which declarations a macro may annotate, so a `function` macro like `{name}` has none"),
        ),
        ProceduralKind::Attribute | ProceduralKind::Derive | ProceduralKind::Wrapper
            if applies_to.is_empty() =>
        {
            reporter.error(
                source,
                span,
                diagnostics::APPLIES_TO_PRESENCE,
                format!("`{name}` annotates a declaration, so it must list the declaration kinds it is legal on with `appliesTo {{ … }}`"),
            );
        }
        _ => {}
    }
    if *trigger_field && !*replace {
        reporter.error(
            source,
            span,
            diagnostics::TRIGGER_WITHOUT_REPLACE,
            format!(
                "`{name}` is summoned by a field annotation, so it rewrites the declaration that \
                 carries the field: it must also declare `replace {{ true }}`"
            ),
        );
    }
    let expected = if kind == ProceduralKind::Wrapper {
        2
    } else {
        1
    };
    let parameter_count = parameters.len();
    if parameter_count != expected {
        reporter.error(
            source,
            span,
            diagnostics::EXPAND_SIGNATURE,
            format!(
                "`{name}` is a `{}` macro, so its `expand` takes {expected} parameter(s), not \
                 {parameter_count}",
                kind_word(kind)
            ),
        );
    }
}
