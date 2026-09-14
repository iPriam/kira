use kira_runtime_abi::Execution;
use kira_source::Span;
use kira_syntax_model::TokenKind;
use kira_syntax_model::ast::{ForeignKind, ForeignMark, Item};

use crate::Parser;

impl Parser<'_> {
    /// Whether the cursor sits on `extern library Name {`.
    pub(super) fn at_extern_library_block(&self) -> bool {
        self.text_of(self.current().span) == "extern"
            && self.peek(1).kind == TokenKind::Identifier
            && self.text_of(self.peek(1).span) == "library"
            && self.peek(2).kind == TokenKind::Identifier
            && self.peek(3).kind == TokenKind::LBrace
    }

    /// Parses grouped C declarations and desugars each member into the same
    /// `ForeignMark` a standalone `@FFI.Extern` declaration carries.
    pub(super) fn parse_extern_library_block(&mut self) {
        let extern_span = self.current().span;
        self.bump(); // `extern`
        self.bump(); // `library`
        let library_span = self.current().span;
        let library = self.intern_span(library_span);
        self.bump(); // library name
        let block_start = self.current().span;
        self.expect(TokenKind::LBrace);

        while !self.at(TokenKind::RBrace) && !self.at_eof() {
            if !self.at(TokenKind::Function) {
                self.error(
                    self.current().span,
                    "KPAR090",
                    "an `extern library` block contains only bodyless `function` declarations",
                );
                self.bump();
                continue;
            }
            let Some(mut function) =
                self.parse_function(false, Execution::Inherited, Some(ForeignKind::Extern))
            else {
                continue;
            };
            let symbol = function.name;
            let symbol_span = function.name_span;
            let library_key = self.intern_text("library", library_span);
            let symbol_key = self.intern_text("symbol", symbol_span);
            let abi_key = self.intern_text("abi", extern_span);
            let c = self.intern_text("c", extern_span);
            function.foreign = Some(ForeignMark {
                kind: ForeignKind::Extern,
                span: extern_span,
                block_span: Span::from_bounds(block_start.start, self.previous_end()),
                fields: vec![
                    kira_syntax_model::ast::ForeignField {
                        key: library_key,
                        key_span: library_span,
                        value: library,
                        value_span: library_span,
                    },
                    kira_syntax_model::ast::ForeignField {
                        key: symbol_key,
                        key_span: symbol_span,
                        value: symbol,
                        value_span: symbol_span,
                    },
                    kira_syntax_model::ast::ForeignField {
                        key: abi_key,
                        key_span: extern_span,
                        value: c,
                        value_span: extern_span,
                    },
                ],
            });
            self.items.push(Item::Function(function));
        }
        self.expect(TokenKind::RBrace);
    }
}
