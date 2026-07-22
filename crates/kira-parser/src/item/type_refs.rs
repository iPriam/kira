//! Type references and the signature pieces every declaration shares.
//!
//! A written type — a name, an array, or a function type — and the parameter
//! list, ownership prefix, and return clause that surround one. Split out of
//! [`super`] so the item grammar there stays about items rather than the types
//! they mention.

use kira_source::Span;
use kira_syntax_model::TokenKind;
use kira_syntax_model::ast::{Param, TypeAliasDecl, TypeRef, TypeRefId};
use kira_syntax_model::ownership::OwnershipMode;

use crate::Parser;

impl Parser<'_> {
    /// Parses `type Name = Target`.
    ///
    /// The target is an ordinary type reference, so `type ByteMatrix =
    /// [[Byte]]` needs no grammar of its own. A missing name yields no node at
    /// all: an alias with nothing to bind would register a name nobody wrote,
    /// and the `type` keyword is already consumed, so recovery still advances.
    pub(crate) fn parse_type_alias(&mut self) -> Option<TypeAliasDecl> {
        let start = self.current().span;
        self.expect(TokenKind::Type);
        if !self.at(TokenKind::Identifier) {
            self.error(self.current().span, "KPAR032", "expected a type alias name");
            return None;
        }
        let name_span = self.current().span;
        let name = self.intern_span(name_span);
        self.bump();
        // `=` is required, not optional: `type Name` on its own aliases nothing.
        self.expect(TokenKind::Equals);
        let target = self.parse_type_ref();
        let span = Span::from_bounds(start.start, self.previous_end());
        Some(TypeAliasDecl {
            name,
            name_span,
            target,
            span,
        })
    }

    // ----- shared signature pieces ---------------------------------------

    pub(crate) fn parse_params(&mut self) -> Vec<Param> {
        let mut params = Vec::new();
        if !self.expect(TokenKind::LParen) {
            return params;
        }
        while !self.at(TokenKind::RParen) && !self.at_eof() {
            let before = self.pos;
            if let Some(param) = self.parse_param() {
                params.push(param);
            }
            if !self.eat(TokenKind::Comma) {
                break;
            }
            if self.pos == before {
                self.bump();
            }
        }
        self.expect(TokenKind::RParen);
        params
    }

    fn parse_param(&mut self) -> Option<Param> {
        if !self.at(TokenKind::Identifier) {
            self.error(self.current().span, "KPAR005", "expected a parameter name");
            return None;
        }
        let name_span = self.current().span;
        let name = self.intern_span(name_span);
        self.bump();
        self.expect(TokenKind::Colon);
        let (ownership, ownership_span) = self.parse_param_ownership();
        let ty = self.parse_type_ref();
        let span = Span::from_bounds(name_span.start, self.previous_end());
        Some(Param {
            name,
            name_span,
            ownership,
            ownership_span,
            ty,
            span,
        })
    }

    /// Parses the ownership prefix of a parameter type, if one is written.
    ///
    /// Accepts `borrow`, `borrow mut`, `move`, and `copy`. All four are
    /// contextual identifiers, so each is committed to only when a type name
    /// follows it — that is what keeps `f(borrow: Int)` (a parameter *named*
    /// `borrow`) and `f(x: move)` (a type named `move`, were one declared)
    /// parsing as they always did. A bare type yields
    /// [`OwnershipMode::Owned`], which is the default rather than a fallback.
    fn parse_param_ownership(&mut self) -> (OwnershipMode, Option<Span>) {
        if self.at_word("borrow") && self.peek_is_word(1, "mut") && self.peek_starts_type(2) {
            let start = self.current().span;
            self.bump(); // `borrow`
            let end = self.current().span;
            self.bump(); // `mut`
            return (
                OwnershipMode::BorrowMut,
                Some(Span::from_bounds(start.start, end.end())),
            );
        }
        if self.at_word("borrow") && self.peek_starts_type(1) {
            let span = self.current().span;
            self.bump();
            return (OwnershipMode::BorrowRead, Some(span));
        }
        for (word, mode) in [("move", OwnershipMode::Move), ("copy", OwnershipMode::Copy)] {
            if self.at_word(word) && self.peek_starts_type(1) {
                let span = self.current().span;
                self.bump();
                return (mode, Some(span));
            }
        }
        (OwnershipMode::Owned, None)
    }

    pub(crate) fn parse_return_type(&mut self) -> Option<TypeRefId> {
        // Kira accepts both `-> Type` and `): Type`.
        if self.eat(TokenKind::Arrow) || self.eat(TokenKind::Colon) {
            Some(self.parse_type_ref())
        } else {
            None
        }
    }

    /// Whether the token `n` ahead can begin a written type.
    ///
    /// A type starts with a name (`Int`, `Point`), with `[` (`[Int]`), or with
    /// `(` (`(Int) -> Void`). This is what every contextual-keyword lookahead
    /// asks, and asking it in one place is what keeps `borrow [Int]` from
    /// silently parsing as a parameter whose type is named `borrow`.
    pub(crate) fn peek_starts_type(&self, n: usize) -> bool {
        matches!(
            self.peek(n).kind,
            TokenKind::Identifier | TokenKind::LBracket | TokenKind::LParen
        )
    }

    /// Parses a written type: a name, `[` element `]`, or a function type,
    /// nested to any depth.
    ///
    /// A name may be **module-qualified** (`Support.Point`). The qualifier is
    /// kept in the interned name — a dot cannot appear in an identifier, so a
    /// qualified spelling can never collide with a declared one — and semantics
    /// is what strips it against the file's imports.
    ///
    /// A leading `(` always starts a function type: no other written type is
    /// parenthesized, so there is nothing to disambiguate against. That is also
    /// why a function result type is spelled with `:` rather than `->` on a
    /// declaration — `function f(): (Int) -> Int` — and both spellings are
    /// accepted for every other result type.
    pub(crate) fn parse_type_ref(&mut self) -> TypeRefId {
        if self.at(TokenKind::LParen) {
            return self.parse_function_type();
        }
        if self.at(TokenKind::LBracket) {
            let start = self.current().span;
            self.bump(); // `[`
            let element = self.parse_type_ref();
            self.expect(TokenKind::RBracket);
            let span = Span::from_bounds(start.start, self.previous_end());
            return self.tree.add_type(TypeRef::Array { element, span });
        }
        if self.at(TokenKind::Identifier) {
            let start = self.current().span;
            let is_any_construct =
                self.text_of(start) == "Any" && self.peek(1).kind == TokenKind::Identifier;
            if is_any_construct {
                self.bump(); // `Any`
                let family_start = self.current().span;
                let mut text = self.text_of(family_start).to_owned();
                self.bump();
                while self.at(TokenKind::Dot) && self.peek(1).kind == TokenKind::Identifier {
                    self.bump(); // `.`
                    let segment = self.current().span;
                    text.push('.');
                    text.push_str(self.text_of(segment));
                    self.bump();
                }
                let family_span = Span::from_bounds(family_start.start, self.previous_end());
                let family = self.intern_text(&text, family_span);
                let span = Span::from_bounds(start.start, self.previous_end());
                return self.tree.add_type(TypeRef::AnyConstruct {
                    family,
                    family_span,
                    span,
                });
            }

            let mut text = self.text_of(start).to_owned();
            self.bump();
            while self.at(TokenKind::Dot) && self.peek(1).kind == TokenKind::Identifier {
                self.bump(); // `.`
                let segment = self.current().span;
                text.push('.');
                text.push_str(self.text_of(segment));
                self.bump();
            }
            let span = Span::from_bounds(start.start, self.previous_end());
            let name = self.intern_text(&text, span);
            // `Name<...>` is a generic instantiation. A type position has no
            // comparison operator, so a `<` here is never ambiguous.
            if self.at_type_params() {
                return self.parse_generic_args(name, span, start);
            }
            return self.tree.add_type(TypeRef::Named { name, span });
        }
        let span = self.current().span;
        self.error(span, "KPAR006", "expected a type name");
        self.tree.add_type(TypeRef::Error { span })
    }

    /// Parses `(A, B) -> R`, with the cursor on `(`.
    ///
    /// The result is mandatory: a function type with no `->` names nothing, so
    /// a missing arrow is reported and the whole type recovers to
    /// [`TypeRef::Error`] rather than silently becoming `() -> Void`.
    fn parse_function_type(&mut self) -> TypeRefId {
        let start = self.current().span;
        self.bump(); // `(`
        let mut params = Vec::new();
        while !self.at(TokenKind::RParen) && !self.at_eof() {
            let before = self.pos;
            params.push(self.parse_type_ref());
            self.eat(TokenKind::Comma);
            // A parameter that consumed nothing would spin; force progress.
            if self.pos == before {
                self.bump();
            }
        }
        self.expect(TokenKind::RParen);
        if !self.eat(TokenKind::Arrow) {
            let span = Span::from_bounds(start.start, self.previous_end());
            self.error(span, "KPAR038", "expected `->` in a function type");
            return self.tree.add_type(TypeRef::Error { span });
        }
        let result = self.parse_type_ref();
        let span = Span::from_bounds(start.start, self.previous_end());
        self.tree.add_type(TypeRef::Function {
            params,
            result,
            span,
        })
    }
}
