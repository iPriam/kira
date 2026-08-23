//! Trait declarations and the `: Trait, Trait` conformance clause.
//!
//! Two productions live here because they are one grammar seen from both ends:
//! [`Parser::parse_trait`] reads what a trait promises, and
//! [`Parser::parse_trait_list`] reads a declaration claiming to keep it. Every
//! declaration form that admits the clause calls the same function, which is
//! what makes `:` mean conformance identically on a struct, a class, and a
//! construct.

use kira_core::Symbol;
use kira_runtime_abi::Execution;
use kira_source::Span;
use kira_syntax_model::TokenKind;
use kira_syntax_model::ast::{TraitDecl, TraitMember, TraitRef};

use crate::Parser;

impl Parser<'_> {
    /// Parses the `: Trait, Trait` conformance clause, or nothing.
    ///
    /// A trailing comma is tolerated as it is everywhere else in this grammar.
    /// Whether the names are traits at all, whether one repeats, and whether
    /// the type may claim them are semantics' questions — every name written is
    /// recorded so each of those answers has a span to point at.
    pub(crate) fn parse_trait_list(&mut self) -> Vec<TraitRef> {
        let mut traits = Vec::new();
        if !self.eat(TokenKind::Colon) {
            return traits;
        }
        loop {
            if !self.at(TokenKind::Identifier) {
                self.error(self.current().span, "KPAR071", "expected a trait name");
                break;
            }
            let span = self.current().span;
            traits.push(TraitRef {
                name: self.intern_span(span),
                span,
            });
            self.bump();
            if !self.eat(TokenKind::Comma) {
                break;
            }
            // A trailing comma before the body or the `extends` clause.
            if self.at(TokenKind::LBrace) || self.at(TokenKind::Extends) {
                break;
            }
        }
        traits
    }

    /// Parses `trait Name { <member>* }`.
    ///
    /// A member is a `function` declaration. Whether it wrote a body is the
    /// whole of what separates a requirement from a default, so no annotation
    /// marks either: a signature alone states an obligation, and a body states
    /// what a conforming type gets when it writes none.
    ///
    /// A trailing `;` after a bodyless member is accepted and consumed, the way
    /// the bodyless foreign form ends.
    pub(crate) fn parse_trait(&mut self) -> Option<TraitDecl> {
        let start = self.current().span;
        self.expect(TokenKind::Trait);
        let (name, name_span) = if self.at(TokenKind::Identifier) {
            let span = self.current().span;
            (self.intern_span(span), span)
        } else {
            self.error(self.current().span, "KPAR072", "expected a trait name");
            (Symbol::ERROR, self.current().span)
        };
        // Consumes the name just interned above.
        if self.at(TokenKind::Identifier) {
            self.bump();
        }
        self.refuse_type_params("trait");
        // Recorded, not refused here: `trait A: B` is a supertrait clause, and
        // naming what it asked for is semantics' to do.
        let supertraits = self.parse_trait_list();
        let mut members = Vec::new();
        if !self.expect(TokenKind::LBrace) {
            let span = Span::from_bounds(start.start, self.previous_end());
            return Some(TraitDecl {
                name,
                name_span,
                supertraits,
                members,
                span,
            });
        }
        while !self.at(TokenKind::RBrace) && !self.at_eof() {
            let before = self.pos;
            while self.eat(TokenKind::Semicolon) {}
            if self.at(TokenKind::RBrace) || self.at_eof() {
                break;
            }
            match self.current_kind() {
                TokenKind::Function => {
                    if let Some(member) = self.parse_trait_member() {
                        members.push(member);
                    }
                }
                kind => {
                    self.error(
                        self.current().span,
                        "KPAR073",
                        format!(
                            "a trait declares `function` members; a signature with no body is a \
                             requirement and one with a body is a default, found {}",
                            kind.describe()
                        ),
                    );
                    self.recover_to_next_trait_member();
                }
            }
            while self.eat(TokenKind::Semicolon) {}
            if self.pos == before {
                self.bump();
            }
        }
        self.expect(TokenKind::RBrace);
        let span = Span::from_bounds(start.start, self.previous_end());
        Some(TraitDecl {
            name,
            name_span,
            supertraits,
            members,
            span,
        })
    }

    /// Parses one trait member, with `function` at the cursor.
    fn parse_trait_member(&mut self) -> Option<TraitMember> {
        let mut function = self.parse_function_signature(false, Execution::Inherited)?;
        let has_body = self.at(TokenKind::LBrace);
        if has_body {
            function.body = self.parse_block();
        } else {
            self.eat(TokenKind::Semicolon);
        }
        function.span = Span::from_bounds(function.span.start, self.previous_end());
        Some(TraitMember { has_body, function })
    }

    /// Skips to the next trait member or the body's closing `}`.
    fn recover_to_next_trait_member(&mut self) {
        if !self.at(TokenKind::RBrace) && !self.at_eof() {
            if self.at(TokenKind::LBrace) {
                self.skip_balanced(TokenKind::LBrace, TokenKind::RBrace);
            } else {
                self.bump();
            }
        }
        while !self.at(TokenKind::RBrace) && !self.at_eof() {
            match self.current_kind() {
                TokenKind::Function => break,
                TokenKind::LBrace => self.skip_balanced(TokenKind::LBrace, TokenKind::RBrace),
                _ => {
                    self.bump();
                }
            }
        }
    }
}
