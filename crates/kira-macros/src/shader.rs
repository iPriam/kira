//! The builtin `ksl!("Shaders/Name.ksl")` shader macro.
//!
//! `ksl!` is the one macro that needs no declaration: it compiles a KSL file at
//! compile time and expands to the artifact literal holding every backend's
//! shader source. That makes it a *macro-shaped* front for the KSL compiler,
//! and the two halves migrate separately — the macro surface (where it may be
//! written, how its argument is validated, what it expands to) is here, and the
//! compilation it delegates to belongs to the KSL pipeline.
//!
//! The KSL pipeline is not implemented in this compiler yet: `kira-ksl-parser`,
//! `kira-ksl-semantics`, `kira-shader-ir`, and the five shader backends are
//! scaffolds. So `ksl!` validates its call site and then refuses with
//! [`KMAC022`], naming what is missing. It never fabricates an artifact — a
//! shader that silently compiled to nothing would take a whole render path down
//! with it at runtime rather than at build time.
//!
//! [`KMAC022`]: crate::diagnostics::SHADER_COMPILE

use kira_syntax_model::TokenKind;

use crate::diagnostics::{self, Reporter};
use crate::invoke::Invocation;
use crate::tokens::Lexed;

/// The name the builtin answers to.
pub(crate) const NAME: &str = "ksl";

/// Whether `name` is the builtin shader macro.
///
/// A user macro of the same name would shadow it, so this is only consulted
/// after the registry has been asked.
pub(crate) fn is_builtin(name: &str) -> bool {
    name == NAME
}

/// Validates one `ksl!(…)` call and reports why it cannot be expanded.
///
/// Returns the expansion when one can be produced, which today is never: the
/// pipeline it needs does not exist yet, and the refusal names that rather than
/// the call site.
pub(crate) fn expand(
    file: &Lexed<'_>,
    call: &Invocation,
    reporter: &mut Reporter,
) -> Option<String> {
    let [argument] = call.arguments.as_slice() else {
        reporter.error(
            file.source,
            call.span,
            diagnostics::SHADER_ARGUMENT_COUNT,
            format!(
                "`{NAME}!` takes one string-literal path to a `.ksl` file, but {} argument(s) \
                 were passed",
                call.arguments.len()
            ),
        );
        return None;
    };
    let Some(path) = string_literal(file, *argument) else {
        reporter.error(
            file.source,
            *argument,
            diagnostics::SHADER_PATH_NOT_LITERAL,
            format!(
                "`{NAME}!` compiles its shader at compile time, so its path must be a string \
                 literal known then"
            ),
        );
        return None;
    };
    reporter.error(
        file.source,
        call.span,
        diagnostics::SHADER_COMPILE,
        format!(
            "`{NAME}!(\"{path}\")` cannot be expanded: this compiler has no KSL pipeline yet, so \
             there is nothing to compile the shader with. The KSL front end and the shader \
             backends are a separate migration; until they land, a shader source has to be \
             supplied to the renderer directly."
        ),
    );
    None
}

/// The decoded contents of `span` when it is a single string literal.
fn string_literal(file: &Lexed<'_>, span: kira_source::Span) -> Option<String> {
    let text = file.slice(span).trim();
    let literal = Lexed::new(file.source, text);
    if literal.kind(0) != TokenKind::StringLiteral || literal.kind(1) != TokenKind::Eof {
        return None;
    }
    Some(kira_lexer::decode_string_literal(text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::invoke;
    use kira_source::SourceId;

    fn expand_first(text: &str) -> Vec<kira_diagnostics::Diagnostic> {
        let file = Lexed::new(SourceId::new(0), text);
        let calls = invoke::find(&file);
        let mut reporter = Reporter::new();
        for call in &calls {
            assert!(expand(&file, call, &mut reporter).is_none());
        }
        reporter.into_diagnostics()
    }

    #[test]
    fn a_literal_path_is_refused_by_the_missing_pipeline() {
        let diagnostics =
            expand_first("function f() {\n    let s = ksl!(\"Shaders/Tri.ksl\")\n}\n");
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code, Some("KMAC022"));
        assert!(
            diagnostics[0].message.contains("Shaders/Tri.ksl"),
            "{:?}",
            diagnostics[0].message
        );
    }

    #[test]
    fn a_non_literal_path_is_refused_before_anything_else() {
        let diagnostics = expand_first("function f() {\n    let s = ksl!(name)\n}\n");
        assert_eq!(diagnostics[0].code, Some("KMAC024"));
    }

    #[test]
    fn the_wrong_argument_count_is_refused() {
        let diagnostics = expand_first("function f() {\n    let s = ksl!(\"a\", \"b\")\n}\n");
        assert_eq!(diagnostics[0].code, Some("KMAC023"));
    }
}
