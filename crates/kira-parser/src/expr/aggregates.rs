//! Array and struct literals: the two bracketed aggregate forms.
//!
//! Split out of [`super`] on the file-size ladder. They belong together: both
//! are a delimiter around a comma-separated list — newlines are whitespace in
//! Kira, so the comma is what separates one element from the next — and both
//! re-enable struct literals inside themselves, because the ambiguity a
//! condition has does not reach past an opening bracket.

use kira_source::Span;
use kira_syntax_model::TokenKind;
use kira_syntax_model::ast::{Expr, ExprId, FieldInit};

use crate::Parser;

impl Parser<'_> {
    /// Whether an array literal's element loop has run out of elements.
    ///
    /// `]` and EOF are the honest ends. `}` is a **recovery** end: a brace can
    /// never appear at an array literal's top level — one inside an element,
    /// as in `[P { x = 1 }]`, is consumed by that element's own parse — so
    /// reaching one means the `[` was never closed. Stopping here bounds an
    /// unclosed literal to its enclosing block instead of letting it swallow
    /// the rest of the file.
    fn at_array_end(&self) -> bool {
        self.at(TokenKind::RBracket) || self.at(TokenKind::RBrace) || self.at_eof()
    }

    /// Parses `[a, b, c]`, with the cursor on `[`.
    ///
    /// Elements are separated by commas and a trailing comma is allowed. Two
    /// elements with nothing between them are refused where the second one
    /// starts: newlines are whitespace, so `[1 2]` and one bare element per
    /// line are the same mistake, and the literal is not the place to guess
    /// where one expression ended and the next began.
    pub(crate) fn parse_array_literal(&mut self) -> ExprId {
        let start = self.current().span;
        self.bump(); // `[`
        let mut elements = Vec::new();
        self.with_struct_literals(|parser| {
            while !parser.at_array_end() {
                let before = parser.pos;
                parser.skip_unknown();
                if parser.at_array_end() {
                    break;
                }
                elements.push(parser.parse_expr());
                if !parser.eat_separator() && !parser.at_array_end() {
                    parser.missing_comma("array elements");
                }
                // An element that consumed nothing would spin; force progress.
                if parser.pos == before {
                    parser.bump();
                }
            }
        });
        self.expect(TokenKind::RBracket);
        let span = Span::from_bounds(start.start, self.previous_end());
        self.tree.add_expr(Expr::ArrayLit { elements, span })
    }

    /// Parses the `{ … }` of a struct literal, with the cursor on `{`.
    ///
    /// Field initializers are separated by commas, with a trailing comma
    /// allowed. Both binders are accepted: `=` is canonical, `:` stays valid
    /// for the transition window, and the two may be mixed in one literal.
    pub(crate) fn parse_struct_literal(
        &mut self,
        name: kira_core::Symbol,
        name_span: Span,
    ) -> ExprId {
        self.bump(); // `{`
        let mut fields = Vec::new();
        // A literal's fields are values, not conditions: a nested literal is
        // legal here even when this one sits in a condition.
        self.with_struct_literals(|parser| {
            while !parser.at(TokenKind::RBrace) && !parser.at_eof() {
                let before = parser.pos;
                parser.skip_unknown();
                if parser.at(TokenKind::RBrace) || parser.at_eof() {
                    break;
                }
                if let Some(field) = parser.parse_field_init() {
                    fields.push(field);
                }
                if !parser.eat_separator() && !parser.at(TokenKind::RBrace) && !parser.at_eof() {
                    parser.missing_comma("field initializers");
                }
                if parser.pos == before {
                    parser.bump();
                }
            }
        });
        self.expect(TokenKind::RBrace);
        let span = Span::from_bounds(name_span.start, self.previous_end());
        self.tree.add_expr(Expr::StructLit {
            name,
            name_span,
            fields,
            span,
        })
    }

    /// Parses one `name = value` / `name: value` field initializer.
    fn parse_field_init(&mut self) -> Option<FieldInit> {
        if !self.at(TokenKind::Identifier) {
            let span = self.current().span;
            self.error(
                span,
                "KPAR023",
                format!(
                    "expected a field name in a struct literal, found {}",
                    self.current_kind().describe()
                ),
            );
            self.bump();
            return None;
        }
        let name_span = self.current().span;
        let name = self.intern_span(name_span);
        self.bump();
        if !self.eat(TokenKind::Equals) && !self.eat(TokenKind::Colon) {
            let span = self.current().span;
            self.error(
                span,
                "KPAR024",
                "expected `=` after a field name in a struct literal",
            );
            return None;
        }
        // A literal's fields are separated by commas, so `secondary:` after a
        // field's own braced value is a missing comma rather than a fill of
        // that value's child slot.
        let value = self.without_named_fills(|parser| parser.parse_expr());
        let span = Span::from_bounds(name_span.start, self.previous_end());
        Some(FieldInit {
            name,
            name_span,
            value,
            span,
        })
    }
}
