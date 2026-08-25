//! Type-parameter lists on a declaration and type-argument lists on a use.
//!
//! # Why only an enum takes type parameters
//!
//! The reference corpus contains exactly one generic declaration —
//! `enum Result<Value, Failure>` — and every other declaration form is written
//! bare. So a `<` after a `struct`, `class`, or `function` name is refused here
//! by name rather than parsed into surface nothing pins: guessing what a
//! generic function means would put a design decision in the parser that no
//! program has ever asked for. The list is still consumed so the rest of the
//! declaration parses and the file reports one mistake instead of a cascade.
//!
//! A parameter may carry a trait bound (`Value: Scored + Send`). The bound is
//! recorded on the parameter and discharged by semantics when an instantiation
//! is minted; the parser only records what was written, so a malformed bound
//! list is the one mistake it reports here (`KPAR079`).
//!
//! # Why `>>` is split rather than rejected
//!
//! The lexer has no idea it is inside a type, so `Result<Result<Int, E>, E>`
//! closes on a single [`TokenKind::GtGt`]. Splitting that token in place — the
//! cursor keeps the first `>` and a fresh one takes its place — is what lets a
//! nested instantiation parse without teaching the lexer about types.

use kira_core::Symbol;
use kira_source::Span;
use kira_syntax_model::Token;
use kira_syntax_model::TokenKind;
use kira_syntax_model::ast::{TraitRef, TypeParamDecl, TypeRef, TypeRefId};

use crate::Parser;

impl Parser<'_> {
    /// Whether a type-parameter or type-argument list starts at the cursor.
    pub(crate) fn at_type_params(&self) -> bool {
        self.at(TokenKind::Lt)
    }

    /// Parses `<A, B>` after a declaration name, with the cursor on `<`.
    ///
    /// Returns the parameters in written order. A parameter may carry a bound
    /// (`Value: Scored + Send`) — see [`Self::parse_param_bounds`]. An empty
    /// list (`<>`) is reported and yields no parameters, which makes the
    /// declaration an ordinary non-generic one rather than a broken generic
    /// one.
    pub(crate) fn parse_type_params(&mut self) -> Vec<TypeParamDecl> {
        let start = self.current().span;
        self.bump(); // `<`
        let mut params: Vec<TypeParamDecl> = Vec::new();
        while !self.at_generic_close() && !self.at_eof() {
            let before = self.pos;
            if self.at(TokenKind::Identifier) {
                let span = self.current().span;
                let name = self.intern_span(span);
                self.bump();
                let bounds = self.parse_param_bounds();
                params.push(TypeParamDecl { name, span, bounds });
            } else {
                let span = self.current().span;
                self.error(
                    span,
                    "KPAR044",
                    format!(
                        "expected a type parameter name, found {}",
                        self.current_kind().describe()
                    ),
                );
                self.bump();
            }
            self.eat(TokenKind::Comma);
            if self.pos == before {
                self.bump();
            }
        }
        if !self.eat_generic_close() {
            let span = Span::from_bounds(start.start, self.previous_end());
            self.error(
                span,
                "KPAR045",
                "expected `>` to close a type parameter list",
            );
            return params;
        }
        if params.is_empty() {
            let span = Span::from_bounds(start.start, self.previous_end());
            self.error(
                span,
                "KPAR046",
                "a type parameter list names at least one parameter",
            );
        }
        params
    }

    /// Parses `: Trait + Trait` after a type-parameter name, or nothing.
    ///
    /// The comma is taken: it separates *parameters*, so the traits of one
    /// parameter's bound are joined with `+`, the way the conformance clause's
    /// own comma list cannot be reused here. A trailing comma is tolerated as
    /// it is everywhere else in this grammar; whether each name is a trait at
    /// all, and whether an argument satisfies it, are semantics' questions —
    /// every name written is recorded so each of those answers has a span to
    /// point at.
    fn parse_param_bounds(&mut self) -> Vec<TraitRef> {
        let mut bounds = Vec::new();
        if !self.eat(TokenKind::Colon) {
            return bounds;
        }
        loop {
            if !self.at(TokenKind::Identifier) {
                self.error(
                    self.current().span,
                    "KPAR079",
                    "expected a trait name in a type parameter's bound",
                );
                break;
            }
            let span = self.current().span;
            bounds.push(TraitRef {
                name: self.intern_span(span),
                span,
            });
            self.bump();
            if !self.eat(TokenKind::Plus) {
                break;
            }
        }
        bounds
    }

    /// Consumes and refuses a type-parameter list on a declaration that may not
    /// take one, naming the construct in the diagnostic.
    ///
    /// Only an `enum` is generic here. `construct` is the keyword the source
    /// wrote, so the message says which declaration is being refused rather
    /// than a generic "not supported".
    pub(crate) fn refuse_type_params(&mut self, construct: &'static str) {
        if !self.at_type_params() {
            return;
        }
        let start = self.current().span;
        self.parse_type_params();
        let span = Span::from_bounds(start.start, self.previous_end());
        self.error(
            span,
            "KPAR047",
            format!(
                "a generic `{construct}` is not supported; only `enum` takes type parameters \
                 (write the concrete types out)"
            ),
        );
    }

    /// Parses `<A, B>` as a call's explicit type arguments.
    pub(crate) fn parse_call_type_args(&mut self) -> Vec<TypeRefId> {
        let start = self.current().span;
        self.bump(); // `<`
        let mut args = Vec::new();
        while !self.at_generic_close() && !self.at_eof() {
            let before = self.pos;
            args.push(self.parse_type_ref());
            self.eat(TokenKind::Comma);
            if self.pos == before {
                self.bump();
            }
        }
        if !self.eat_generic_close() {
            let span = Span::from_bounds(start.start, self.previous_end());
            self.error(
                span,
                "KPAR045",
                "expected `>` to close a type argument list",
            );
        }
        if args.is_empty() {
            let span = Span::from_bounds(start.start, self.previous_end());
            self.error(
                span,
                "KPAR046",
                "a type argument list names at least one type",
            );
        }
        args
    }

    /// Parses `<A, B>` after a type name in a type position, with the cursor on
    /// `<`.
    ///
    /// `name`/`name_span` describe the name already consumed, and `start` is
    /// where that name began, so the node's span covers the whole
    /// instantiation.
    pub(crate) fn parse_generic_args(
        &mut self,
        name: Symbol,
        name_span: Span,
        start: Span,
    ) -> TypeRefId {
        self.bump(); // `<`
        let mut args: Vec<TypeRefId> = Vec::new();
        while !self.at_generic_close() && !self.at_eof() {
            let before = self.pos;
            args.push(self.parse_type_ref());
            self.eat(TokenKind::Comma);
            if self.pos == before {
                self.bump();
            }
        }
        if !self.eat_generic_close() {
            let span = Span::from_bounds(start.start, self.previous_end());
            self.error(
                span,
                "KPAR045",
                "expected `>` to close a type argument list",
            );
            return self.tree.add_type(TypeRef::Error { span });
        }
        let span = Span::from_bounds(start.start, self.previous_end());
        if args.is_empty() {
            self.error(
                span,
                "KPAR046",
                "a type argument list names at least one type",
            );
            return self.tree.add_type(TypeRef::Error { span });
        }
        self.tree.add_type(TypeRef::Generic {
            name,
            name_span,
            args,
            span,
        })
    }

    /// Whether the cursor is on a `>` that closes a generic list — including
    /// the first half of a `>>`.
    fn at_generic_close(&self) -> bool {
        matches!(self.current_kind(), TokenKind::Gt | TokenKind::GtGt)
    }

    /// Consumes one closing `>`, splitting a `>>` in place when that is what
    /// the lexer produced.
    ///
    /// Splitting rewrites the `>>` token as its first `>` and inserts its
    /// second, so the cursor advances past exactly one of them and every span —
    /// including [`Parser::previous_end`] — stays byte-accurate.
    fn eat_generic_close(&mut self) -> bool {
        match self.current_kind() {
            TokenKind::Gt => {
                self.bump();
                true
            }
            TokenKind::GtGt => {
                let span = self.current().span;
                let first = Span::new(span.start, 1);
                let second = Span::new(span.start.saturating_add(1), 1);
                self.tokens[self.pos] = Token::new(TokenKind::Gt, first);
                self.tokens
                    .insert(self.pos + 1, Token::new(TokenKind::Gt, second));
                self.bump();
                true
            }
            _ => false,
        }
    }
}
