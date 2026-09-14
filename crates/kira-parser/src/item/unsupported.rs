use kira_diagnostics::{Code, Diagnostic, Label, Severity};
use kira_source::{FileSpan, Span};
use kira_syntax_model::TokenKind;
use kira_syntax_model::ast::{Item, UnsupportedItem};

use crate::Parser;

impl Parser<'_> {
    /// Parses an unsupported top-level item while preserving the recovery boundary.
    pub(super) fn parse_unsupported_item(&mut self) {
        let start = self.current().span;
        self.parse_unsupported_item_from(start);
    }

    /// Continues unsupported-item recovery from an already captured starting span.
    pub(super) fn parse_unsupported_item_from(&mut self, start: Span) {
        let keyword = unsupported_keyword(self.current_kind(), self.text_of(self.current().span));
        // Walk forward: if a `{...}` body appears before the next top-level
        // starter, consume it balanced; otherwise stop at the next starter.
        while !self.at_eof() {
            match self.current_kind() {
                TokenKind::LBrace => {
                    self.skip_balanced(TokenKind::LBrace, TokenKind::RBrace);
                    break;
                }
                kind if is_item_start(kind) && self.current().span != start => break,
                _ => {
                    self.bump();
                }
            }
        }
        let span = Span::from_bounds(start.start, self.previous_end());
        self.items
            .push(Item::Unsupported(UnsupportedItem { keyword, span }));
        let file_span = FileSpan::new(self.source, span);
        let mut diagnostic = Diagnostic::single(
            Severity::Error,
            format!("`{keyword}` is not supported yet"),
            Label::primary(file_span, "not yet supported in this compiler"),
        );
        diagnostic.code = Some(Code::known("KSEM900"));
        diagnostic.phase = Some("parser");
        diagnostic.help = Some(
            "the v0 subset supports functions, structs, let/var, if/while, and arithmetic"
                .to_owned(),
        );
        self.diagnostics.push(diagnostic);
    }
}

/// Whether `kind` can begin a top-level item, used to bound error recovery.
fn is_item_start(kind: TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::At
            | TokenKind::Function
            | TokenKind::Struct
            | TokenKind::Enum
            | TokenKind::Type
            | TokenKind::Class
            | TokenKind::Construct
            | TokenKind::Trait
            | TokenKind::Import
    )
}

/// A stable label for an unsupported construct, for diagnostics.
fn unsupported_keyword(kind: TokenKind, text: &str) -> &'static str {
    match kind {
        TokenKind::Enum => "enum",
        TokenKind::Class => "class",
        TokenKind::Import => "import",
        // `Package` is a real declaration form this parser has not built.
        // Every other identifier-led form is an ordinary name: a declaration
        // backed by a family is written `construct Name(…) extends Family`, so
        // no identifier begins one.
        TokenKind::Identifier => match text {
            "Package" => "Package",
            _ => "declaration",
        },
        _ => "declaration",
    }
}
