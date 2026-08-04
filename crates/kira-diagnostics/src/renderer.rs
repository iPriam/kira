//! Human-readable diagnostic rendering: header, location, source excerpt, and
//! a caret underline beneath each labelled span.
//!
//! Output shape (rustc-like):
//!
//! ```text
//! error[KSEM060]: undefined name `x`
//!  --> test.kira:3:11
//!   |
//! 3 |     print(x)
//!   |           ^ undefined name `x`
//! ```
//!
//! A span covering more than one line is drawn as a bracket down the left of
//! the excerpt rather than a caret run, because a caret run cannot cross a line
//! and clamping one to the first line would claim the problem ends there:
//!
//! ```text
//! warning[KLINT002]: this `while` counts an index by hand
//!   --> total.kira:4:5
//!    |
//!  4 |       while index < xs.count {
//!    |  _____^
//!  5 | |         index = index + 1
//!  6 | |     }
//!    | |_____^ this `while` counts an index by hand
//! ```

use crate::diagnostic::{Diagnostic, Severity};
use crate::label::{Label, LabelKind};
use kira_source::SourceMap;

/// How many excerpt lines a multi-line span shows before its middle is elided.
///
/// A lint about a whole function has a span as long as the function, and
/// printing four hundred lines to underline them all buries the message. The
/// two ends are what a reader needs; the middle is what they already have open.
const MAX_EXCERPT_LINES: usize = 8;

/// Renders one diagnostic against its sources into a multi-line string.
pub fn render(diagnostic: &Diagnostic, sources: &SourceMap) -> String {
    let mut out = String::new();
    render_header(diagnostic, &mut out);
    // Primary first, then every secondary in the order it was attached: the
    // main site is what the reader looks at, and the context explains it.
    for label in diagnostic
        .labels
        .iter()
        .filter(|label| label.kind == LabelKind::Primary)
        .chain(
            diagnostic
                .labels
                .iter()
                .filter(|label| label.kind == LabelKind::Secondary),
        )
    {
        render_label(sources, label, &mut out);
    }
    for note in &diagnostic.notes {
        out.push_str(&format!("  = note: {note}\n"));
    }
    if let Some(help) = &diagnostic.help {
        out.push_str(&format!("  = help: {help}\n"));
    }
    // Last, because it is the line a reader acts on: `help` says what to do and
    // `fix` says whether a tool can do it for them.
    if let Some(suggestion) = &diagnostic.suggestion {
        out.push_str(&format!("  = fix: {}\n", suggestion.message));
    }
    out
}

fn render_header(diagnostic: &Diagnostic, out: &mut String) {
    let severity = severity_word(diagnostic.severity);
    match &diagnostic.code {
        Some(code) => out.push_str(&format!("{severity}[{code}]: {}\n", diagnostic.message)),
        None => out.push_str(&format!("{severity}: {}\n", diagnostic.message)),
    }
}

/// Renders one label's location and excerpt.
fn render_label(sources: &SourceMap, label: &Label, out: &mut String) {
    let span = label.span;
    if span.source.value() as usize >= sources.len() {
        return;
    }
    let file = sources.get(span.source);
    let start = file.line_map.line_column(span.span.start);
    let first = file.line_map.line_index(span.span.start);
    // A zero-length span ends where it starts; anything else ends on the last
    // byte it covers, not the one after it, or a span stopping at a newline
    // would claim the following line.
    let last_byte = span.span.end().saturating_sub(1).max(span.span.start);
    let last = file.line_map.line_index(last_byte);

    let gutter = (start.line as usize).max(last + 1).to_string().len();
    let pad = " ".repeat(gutter);

    out.push_str(&format!(
        " {}--> {}:{}:{}\n",
        pad, file.path, start.line, start.column
    ));
    out.push_str(&format!("{pad} |\n"));

    let line_text = |index: usize| {
        let (from, to) = file.line_map.line_bounds(index, &file.text);
        file.text
            .get(from as usize..to as usize)
            .unwrap_or("")
            .to_owned()
    };

    if first == last {
        let text = line_text(first);
        out.push_str(&format!("{:>gutter$} | {text}\n", start.line));
        let caret_col = start.column.saturating_sub(1) as usize;
        let available = text.len().saturating_sub(caret_col);
        let caret_len = (span.span.len as usize).clamp(1, available.max(1));
        let underline = format!("{}{}", " ".repeat(caret_col), "^".repeat(caret_len));
        out.push_str(&format!("{pad} | {underline} {}\n", label.message));
        return;
    }

    // Multi-line: open a bracket under the start, run it down the excerpt, and
    // close it under the last line.
    let text = line_text(first);
    out.push_str(&format!("{:>gutter$} |   {text}\n", start.line));
    let caret_col = start.column.saturating_sub(1) as usize;
    out.push_str(&format!("{pad} |  {}^\n", "_".repeat(caret_col + 1)));

    let body: Vec<usize> = ((first + 1)..=last).collect();
    let elide = body.len() > MAX_EXCERPT_LINES;
    for (position, &index) in body.iter().enumerate() {
        if elide && position == MAX_EXCERPT_LINES / 2 {
            out.push_str("...\n");
            continue;
        }
        if elide
            && position > MAX_EXCERPT_LINES / 2
            && position < body.len() - MAX_EXCERPT_LINES / 2
        {
            continue;
        }
        let number = index + 1;
        out.push_str(&format!("{number:>gutter$} | | {}\n", line_text(index)));
    }
    out.push_str(&format!("{pad} | |{}^ {}\n", "_".repeat(4), label.message));
}

fn severity_word(severity: Severity) -> &'static str {
    match severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Note => "note",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostic::{Code, Diagnostic, Suggestion};
    use crate::label::Label;
    use kira_source::{FileSpan, Span};

    #[test]
    fn renders_location_and_caret() {
        let mut sources = SourceMap::new();
        let id = sources
            .insert(
                "test.kira".to_owned(),
                "@Main\nfunction main() { print(x) }".to_owned(),
            )
            .expect("an empty map takes a file");
        // `x` sits inside the second line.
        let offset = "@Main\nfunction main() { print(".len() as u32;
        let span = FileSpan::new(id, Span::new(offset, 1));
        let mut diagnostic = Diagnostic::single(
            Severity::Error,
            "undefined name `x`",
            Label::primary(span, "undefined name `x`"),
        );
        diagnostic.code = Some(Code::known("KSEM060"));
        let rendered = render(&diagnostic, &sources);
        assert!(rendered.contains("error[KSEM060]: undefined name `x`"));
        assert!(rendered.contains("test.kira:2:"));
        assert!(rendered.contains('^'));
    }

    #[test]
    fn a_lint_renders_its_code_help_and_fix() {
        let mut sources = SourceMap::new();
        let text = "match colour {\n    Red -> return\n    _ => return\n}\n";
        let id = sources
            .insert("State.kira".to_owned(), text.to_owned())
            .expect("an empty map takes a file");
        let offset = text.find("_ =>").map_or(0, |at| at as u32);
        let span = FileSpan::new(id, Span::new(offset, 1));
        let mut diagnostic = Diagnostic::single(
            Severity::Warning,
            "redundant catch-all pattern",
            Label::primary(span, "this enum is already matched exhaustively"),
        );
        diagnostic.code = Some(Code::known("KLINT014"));
        diagnostic.help = Some("remove this arm".to_owned());
        diagnostic.suggestion = Some(Suggestion::removal("safely removable", span));

        let rendered = render(&diagnostic, &sources);
        assert!(
            rendered.contains("warning[KLINT014]: redundant catch-all pattern"),
            "{rendered}"
        );
        assert!(rendered.contains("--> State.kira:3:5"), "{rendered}");
        assert!(
            rendered.contains("^ this enum is already matched exhaustively"),
            "{rendered}"
        );
        assert!(rendered.contains("= help: remove this arm"), "{rendered}");
        assert!(rendered.contains("= fix: safely removable"), "{rendered}");
    }

    #[test]
    fn a_code_named_at_run_time_renders_like_a_cataloged_one() {
        let mut sources = SourceMap::new();
        let id = sources
            .insert("State.kira".to_owned(), "let x = 1\n".to_owned())
            .expect("an empty map takes a file");
        let span = FileSpan::new(id, Span::new(0, 3));
        let mut diagnostic = Diagnostic::single(
            Severity::Warning,
            "a lint said so",
            Label::primary(span, "here"),
        );
        // Built from a String, as a lint written in Kira must be: there is no
        // `&'static str` for a code the compiler has never heard of.
        let owned = String::from("KLINT") + "099";
        diagnostic.code = Some(Code::named(owned));

        assert!(diagnostic.has_code("KLINT099"));
        assert_eq!(diagnostic.code_text(), Some("KLINT099"));
        assert!(
            render(&diagnostic, &sources).contains("warning[KLINT099]: a lint said so"),
            "a run-time code renders exactly like a cataloged one"
        );
    }

    #[test]
    fn a_fix_says_whether_a_tool_may_apply_it() {
        let span = FileSpan::new(kira_source::SourceId::new(0), Span::new(0, 3));
        assert!(Suggestion::removal("safely removable", span).is_machine_applicable());
        assert!(Suggestion::rewrite("write `.Red`", span, ".Red").is_machine_applicable());
        // Nothing may be applied unattended unless it says so.
        let unsure = Suggestion {
            message: "maybe drop this".to_owned(),
            span,
            replacement: String::new(),
            applicability: crate::diagnostic::Applicability::MaybeIncorrect,
        };
        assert!(!unsure.is_machine_applicable());
    }

    #[test]
    fn a_span_crossing_lines_is_bracketed_rather_than_clamped() {
        let mut sources = SourceMap::new();
        let text = "function total() {\n    while i < n {\n        i = i + 1\n    }\n}\n";
        let id = sources
            .insert("total.kira".to_owned(), text.to_owned())
            .expect("an empty map takes a file");
        let start = text.find("while").map_or(0, |at| at as u32);
        let end = text.find("    }\n}").map_or(0, |at| at as u32) + 5;
        let span = FileSpan::new(id, Span::new(start, end - start));
        let diagnostic = Diagnostic::single(
            Severity::Warning,
            "this `while` counts an index by hand",
            Label::primary(span, "counts an index by hand"),
        );

        let rendered = render(&diagnostic, &sources);
        // Every line the span covers is shown, and the bracket closes under the
        // last one rather than a caret run stopping at the first.
        assert!(rendered.contains("while i < n {"), "{rendered}");
        assert!(rendered.contains("i = i + 1"), "{rendered}");
        assert!(rendered.contains("^ counts an index by hand"), "{rendered}");
        assert!(
            rendered.contains("| |"),
            "the bracket runs down: {rendered}"
        );
    }

    #[test]
    fn a_secondary_label_is_rendered_after_the_primary() {
        let mut sources = SourceMap::new();
        let text = "enum Colour {\n    Red\n}\n\nmatch c {\n    _ => return\n}\n";
        let id = sources
            .insert("State.kira".to_owned(), text.to_owned())
            .expect("an empty map takes a file");
        let arm = text.find("_ =>").map_or(0, |at| at as u32);
        let declared = text.find("enum Colour").map_or(0, |at| at as u32);
        let mut diagnostic = Diagnostic::single(
            Severity::Warning,
            "redundant catch-all pattern",
            Label::primary(FileSpan::new(id, Span::new(arm, 1)), "this arm is dead"),
        );
        diagnostic.labels.push(Label::secondary(
            FileSpan::new(id, Span::new(declared, 4)),
            "every variant of this enum is already covered",
        ));

        let rendered = render(&diagnostic, &sources);
        let primary = rendered.find("this arm is dead").expect("the primary");
        let secondary = rendered
            .find("every variant of this enum is already covered")
            .expect("the secondary");
        assert!(primary < secondary, "primary comes first: {rendered}");
    }
}
