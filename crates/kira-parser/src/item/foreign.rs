//! Parsing the `@FFI.Extern { ... }` annotation block.
//!
//! The one qualified annotation the grammar knows: its `root.member` head, and
//! the `{ key: value; ... }` field block underneath. Every structural mistake
//! is reported and recovered from here; field *meaning* — required, duplicate,
//! unknown, the `abi` value — is the analyzer's, not the parser's. Split out of
//! [`super`] so the item grammar there is not interleaved with the foreign one.

use kira_source::Span;
use kira_syntax_model::TokenKind;
use kira_syntax_model::ast::{ForeignField, ForeignMark};

use super::Annotations;
use crate::Parser;

impl Parser<'_> {
    /// Parses a qualified annotation — the cursor is on its first identifier and
    /// a `. identifier` follows. Only `@FFI.Extern { ... }` is known.
    pub(crate) fn parse_qualified_annotation(&mut self, annotations: &mut Annotations) {
        let root_span = self.current().span;
        let root = self.text_of(root_span).to_owned();
        let member_span = self.peek(2).span;
        let member = self.text_of(member_span).to_owned();
        let name_span = Span::from_bounds(root_span.start, member_span.end());
        self.bump(); // root identifier
        self.bump(); // `.`
        self.bump(); // member identifier
        if root != "FFI" || member != "Extern" {
            self.error(
                name_span,
                "KPAR053",
                format!(
                    "unknown qualified annotation `@{root}.{member}`; only `@FFI.Extern` exists"
                ),
            );
            // Skip a `{ ... }` payload so recovery lands on the declaration.
            if self.at(TokenKind::LBrace) {
                self.skip_balanced(TokenKind::LBrace, TokenKind::RBrace);
            }
            return;
        }
        let block_start = self.current().span.start;
        let fields = self.parse_foreign_block();
        let block_end = self.previous_end();
        // When no `{` was there to consume, nothing advanced, so the block has
        // no span of its own; the annotation name is what a diagnostic points
        // at instead.
        let block_span = if block_end >= block_start {
            Span::from_bounds(block_start, block_end)
        } else {
            name_span
        };
        annotations.foreign = Some(ForeignMark {
            span: name_span,
            block_span,
            fields,
        });
    }

    /// Parses the `{ key: value; ... }` block of an `@FFI.Extern` annotation.
    ///
    /// Each field is `identifier : identifier ;`. Every structural mistake — a
    /// missing brace, a non-identifier key, a missing colon, a non-identifier
    /// value, a missing terminator — is reported with its own code, and recovery
    /// advances to the next field so one bad field does not swallow the rest.
    /// Field *meaning* (required, duplicate, unknown, the `abi` value) is the
    /// analyzer's, not the parser's.
    fn parse_foreign_block(&mut self) -> Vec<ForeignField> {
        let mut fields = Vec::new();
        if !self.at(TokenKind::LBrace) {
            self.error(
                self.current().span,
                "KPAR048",
                "expected `{` to open the `@FFI.Extern` block",
            );
            return fields;
        }
        self.bump(); // `{`
        while !self.at(TokenKind::RBrace) && !self.at_eof() {
            let before = self.pos;
            if let Some(field) = self.parse_foreign_field() {
                fields.push(field);
            }
            // Force progress: a field that consumed nothing would spin.
            if self.pos == before {
                self.bump();
            }
        }
        self.expect(TokenKind::RBrace);
        fields
    }

    /// Parses one `identifier : identifier ;` field, or reports why it could
    /// not and returns `None`.
    fn parse_foreign_field(&mut self) -> Option<ForeignField> {
        if !self.at(TokenKind::Identifier) {
            self.error(
                self.current().span,
                "KPAR049",
                "expected a field name in the `@FFI.Extern` block",
            );
            return None;
        }
        let key_span = self.current().span;
        let key = self.intern_span(key_span);
        self.bump();
        if !self.at(TokenKind::Colon) {
            self.error(
                self.current().span,
                "KPAR050",
                "expected `:` after an `@FFI.Extern` field name",
            );
            return None;
        }
        self.bump(); // `:`
        if !self.at(TokenKind::Identifier) {
            self.error(
                self.current().span,
                "KPAR051",
                "expected a field value in the `@FFI.Extern` block",
            );
            return None;
        }
        let value_span = self.current().span;
        let value = self.intern_span(value_span);
        self.bump();
        if !self.at(TokenKind::Semicolon) {
            self.error(
                self.current().span,
                "KPAR052",
                "expected `;` after an `@FFI.Extern` field",
            );
            return Some(ForeignField {
                key,
                key_span,
                value,
                value_span,
            });
        }
        self.bump(); // `;`
        Some(ForeignField {
            key,
            key_span,
            value,
            value_span,
        })
    }
}
