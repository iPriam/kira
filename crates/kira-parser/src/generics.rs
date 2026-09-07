//! Type-parameter lists on a declaration and type-argument lists on a use.
//!
//! Type parameters are shared by every declaration that owns a type-level
//! signature: enums, structs, classes, functions, and traits. A declaration
//! remains a template until semantics substitutes a complete argument list, so
//! the parser records the names and bounds without trying to decide what a
//! concrete type means.
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
use kira_syntax_model::ast::{Function, TraitRef, TypeParamDecl, TypeRef, TypeRefId};

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
            let name = self.intern_span(span);
            self.bump();
            let args = if self.at(TokenKind::Lt) {
                self.parse_call_type_args()
            } else {
                Vec::new()
            };
            bounds.push(TraitRef { name, span, args });
            if !self.eat(TokenKind::Plus) {
                break;
            }
        }
        bounds
    }

    /// Consumes and refuses a type-parameter list on a declaration member that
    /// may not take one, naming the construct in the diagnostic.
    ///
    /// Construct lifecycle and requirement members do not own an independent
    /// specialization, even though ordinary top-level functions and aggregate
    /// methods do. `construct` is the keyword the source wrote, so the message
    /// names the form rather than giving a generic "not supported" refusal.
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
                "a generic `{construct}` member is not supported; write the concrete types out"
            ),
        );
    }

    /// Refuses type parameters on a member function after its complete body or
    /// signature has been parsed. Only free functions own an independent
    /// callable specialization; a method is specialized by its enclosing
    /// aggregate or trait instead, so a second parameter list would have no
    /// place in the semantic tables.
    pub(crate) fn refuse_generic_member(&mut self, function: &mut Function) {
        let Some(first) = function.type_params.first() else {
            return;
        };
        let last = function
            .type_params
            .last()
            .expect("the first type parameter implies a last one");
        let span = Span::from_bounds(first.span.start, last.span.end());
        self.error(
            span,
            "KPAR047",
            "a generic declaration member is not supported; put type parameters on the enclosing declaration or write a free function",
        );
        let parameter_spans: Vec<Span> = function
            .params
            .iter()
            .map(|param| self.tree.type_ref(param.ty).span())
            .collect();
        for (param, type_span) in function.params.iter_mut().zip(parameter_spans) {
            param.ty = self.tree.add_type(TypeRef::Error { span: type_span });
        }
        if let Some(return_type) = &mut function.return_type {
            let type_span = self.tree.type_ref(*return_type).span();
            *return_type = self.tree.add_type(TypeRef::Error { span: type_span });
        }
        function.type_params.clear();
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
