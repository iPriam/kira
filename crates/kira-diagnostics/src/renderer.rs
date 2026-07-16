//! Human-readable diagnostic rendering: header, location, source excerpt, and
//! a caret underline beneath the primary span.
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

use crate::diagnostic::{Diagnostic, Severity};
use kira_source::SourceMap;

/// Renders one diagnostic against its sources into a multi-line string.
pub fn render(diagnostic: &Diagnostic, sources: &SourceMap) -> String {
    let mut out = String::new();
    render_header(diagnostic, &mut out);
    if let Some(label) = diagnostic.primary_label() {
        render_snippet(sources, label.span, &label.message, &mut out);
    }
    for note in &diagnostic.notes {
        out.push_str(&format!("  = note: {note}\n"));
    }
    if let Some(help) = &diagnostic.help {
        out.push_str(&format!("  = help: {help}\n"));
    }
    out
}

fn render_header(diagnostic: &Diagnostic, out: &mut String) {
    let severity = severity_word(diagnostic.severity);
    match diagnostic.code {
        Some(code) => out.push_str(&format!("{severity}[{code}]: {}\n", diagnostic.message)),
        None => out.push_str(&format!("{severity}: {}\n", diagnostic.message)),
    }
}

fn render_snippet(sources: &SourceMap, span: kira_source::FileSpan, label: &str, out: &mut String) {
    if span.source.value() as usize >= sources.len() {
        return;
    }
    let file = sources.get(span.source);
    let position = file.line_map.line_column(span.span.start);
    let line_index = file.line_map.line_index(span.span.start);
    let (line_start, line_end) = file.line_map.line_bounds(line_index, &file.text);
    let line_text = &file.text[line_start as usize..line_end as usize];

    let gutter = position.line.to_string();
    let pad = " ".repeat(gutter.len());

    out.push_str(&format!(
        " {}--> {}:{}:{}\n",
        pad, file.path, position.line, position.column
    ));
    out.push_str(&format!("{pad} |\n"));
    out.push_str(&format!("{gutter} | {line_text}\n"));

    // Caret run under the span, clamped to the line's length.
    let caret_col = position.column.saturating_sub(1) as usize;
    let available = line_text.len().saturating_sub(caret_col);
    let caret_len = (span.span.len as usize).clamp(1, available.max(1));
    let underline = format!("{}{}", " ".repeat(caret_col), "^".repeat(caret_len));
    out.push_str(&format!("{pad} | {underline} {label}\n"));
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
    use crate::diagnostic::Diagnostic;
    use crate::label::Label;
    use kira_source::{FileSpan, Span};

    #[test]
    fn renders_location_and_caret() {
        let mut sources = SourceMap::new();
        let id = sources.insert(
            "test.kira".to_owned(),
            "@Main\nfunction main() { print(x) }".to_owned(),
        );
        // `x` sits inside the second line.
        let offset = "@Main\nfunction main() { print(".len() as u32;
        let span = FileSpan::new(id, Span::new(offset, 1));
        let mut diagnostic = Diagnostic::single(
            Severity::Error,
            "undefined name `x`",
            Label::primary(span, "undefined name `x`"),
        );
        diagnostic.code = Some("KSEM060");
        let rendered = render(&diagnostic, &sources);
        assert!(rendered.contains("error[KSEM060]: undefined name `x`"));
        assert!(rendered.contains("test.kira:2:"));
        assert!(rendered.contains('^'));
    }
}
