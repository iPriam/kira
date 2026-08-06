//! Construct declaration-family parsing: the family template
//! `construct Family { ... }` and the construct-backed declaration
//! `Family Name(params) { ... }` that conforms to one.
//!
//! Both share a member body — stored `let` members, computed block-bodied
//! members (`let node: Any { expr }`), `function` members, and the bodyless
//! `@Required function f(…) -> T` requirement — so they share a parser. A family
//! adds nothing to the header beyond its name; a backed declaration adds a
//! function-style parameter list, which becomes its construction inputs.
//!
//! Members and clauses the executable slice does not cover yet (`@Content`
//! slots, `@Consuming` methods, `extends`/`requires` inheritance) are parsed
//! into a [`DeferredConstruct`] rather than dropped, so semantics refuses each
//! with a precise typed diagnostic instead of the generic parse-don't-crash
//! node.

use kira_core::Symbol;
use kira_runtime_abi::Execution;
use kira_source::Span;
use kira_syntax_model::TokenKind;
use kira_syntax_model::ast::{
    ConstructDecl, ConstructField, ConstructKind, ConstructMethod, DeferredConstruct, ExtendDecl,
    TypeRef,
};

use crate::Parser;

mod members;

/// The members parsed out of a construct body, before they are placed in a
/// [`ConstructDecl`].
#[derive(Default)]
struct ConstructBody {
    fields: Vec<ConstructField>,
    methods: Vec<ConstructMethod>,
    deferred: Vec<DeferredConstruct>,
}

impl Parser<'_> {
    /// Whether the cursor begins a construct-backed declaration:
    /// `Family Name(` or `Family Name {`. The leading identifier is the family
    /// name, so this is what tells a backed declaration apart from a stray
    /// identifier at top level.
    pub(crate) fn at_construct_backed(&self) -> bool {
        self.at(TokenKind::Identifier)
            && self.peek(1).kind == TokenKind::Identifier
            && matches!(self.peek(2).kind, TokenKind::LParen | TokenKind::LBrace)
    }

    /// Whether the cursor begins an `extend Family { ... }` block.
    ///
    /// `extend` is a **contextual** keyword, not a reserved word: it is
    /// recognized only here, by the leading identifier's text, so an ordinary
    /// name spelled `extend` elsewhere is untouched. The `{` after the family
    /// name is what tells the block apart from a construct-backed declaration
    /// (`Family Name {`), whose second identifier is the declaration name — this
    /// check must therefore run before [`at_construct_backed`](Self::at_construct_backed).
    pub(crate) fn at_extend_block(&self) -> bool {
        self.at(TokenKind::Identifier)
            && self.text_of(self.current().span) == "extend"
            && self.peek(1).kind == TokenKind::Identifier
            && self.peek(2).kind == TokenKind::LBrace
    }

    /// Parses `extend Family { [@Native] function ... }`, with `extend` at the
    /// cursor.
    ///
    /// The block holds only `function` modifiers, annotated or not; any other
    /// member is refused with a typed diagnostic and skipped, so one malformed
    /// member does not cascade. The family name and modifier bodies are what
    /// semantics needs — each modifier lowers to a function whose receiver is
    /// the family value.
    pub(crate) fn parse_extend(&mut self) -> Option<ExtendDecl> {
        let start = self.current().span;
        self.bump(); // `extend`
        let name_span = self.current().span;
        let name = if self.at(TokenKind::Identifier) {
            let symbol = self.intern_span(name_span);
            self.bump();
            symbol
        } else {
            self.error(
                name_span,
                "KPAR063",
                "expected the name of the construct family to extend",
            );
            Symbol::ERROR
        };
        let mut methods = Vec::new();
        self.expect(TokenKind::LBrace);
        while !self.at(TokenKind::RBrace) && !self.at_eof() {
            let before = self.pos;
            while self.eat(TokenKind::Semicolon) {}
            if self.at(TokenKind::RBrace) || self.at_eof() {
                break;
            }
            match self.current_kind() {
                TokenKind::Function => {
                    if let Some(function) = self.parse_function(false, Execution::Inherited, false)
                    {
                        methods.push(function);
                    }
                }
                // An annotated modifier. `@Native` and `@Runtime` select the
                // engine a modifier's body runs on, exactly as they do for a
                // free function or a class method — a modifier is a function,
                // and the block it is written in does not change that. Every
                // other annotation is recorded and refused by name where the
                // modifier is registered, rather than as a syntax error about
                // the wrong thing.
                TokenKind::At => {
                    let annotations = self.parse_annotations();
                    if self.at(TokenKind::Function) {
                        if let Some(function) = self.parse_function_annotated(&annotations) {
                            methods.push(function);
                        }
                    } else {
                        let span = self.current().span;
                        self.error(
                            span,
                            "KPAR064",
                            "expected `function` after an annotation in an `extend` block",
                        );
                        self.recover_to_next_construct_member();
                    }
                }
                kind => {
                    let span = self.current().span;
                    self.error(
                        span,
                        "KPAR064",
                        format!(
                            "an `extend` block holds only `function` modifiers, found {}",
                            kind.describe()
                        ),
                    );
                    self.recover_to_next_construct_member();
                }
            }
            while self.eat(TokenKind::Semicolon) {}
            if self.pos == before {
                self.bump();
            }
        }
        self.expect(TokenKind::RBrace);
        let span = Span::from_bounds(start.start, self.previous_end());
        Some(ExtendDecl {
            name,
            name_span,
            methods,
            span,
        })
    }

    /// Parses `construct Family [extends ...] { <member>* }`.
    pub(crate) fn parse_construct_family(&mut self) -> Option<ConstructDecl> {
        let start = self.current().span;
        self.expect(TokenKind::Construct);
        let (name, name_span) = if self.at(TokenKind::Identifier) {
            let span = self.current().span;
            (self.intern_span(span), span)
        } else {
            self.error(self.current().span, "KPAR057", "expected a construct name");
            (Symbol::ERROR, self.current().span)
        };
        if self.at(TokenKind::Identifier) {
            self.bump();
        }
        let mut deferred = Vec::new();
        self.skip_construct_header_clauses(&mut deferred);
        let mut body = ConstructBody {
            deferred,
            ..ConstructBody::default()
        };
        self.parse_construct_body(&mut body, None);
        let span = Span::from_bounds(start.start, self.previous_end());
        Some(ConstructDecl {
            kind: ConstructKind::Family,
            name,
            name_span,
            fields: body.fields,
            methods: body.methods,
            deferred: body.deferred,
            span,
        })
    }

    /// Parses `Family Name(params) { <member>* }`, with the family name at the
    /// cursor.
    pub(crate) fn parse_construct_backed(&mut self) -> Option<ConstructDecl> {
        let start = self.current().span;
        let family_span = self.current().span;
        let family = self.intern_span(family_span);
        self.bump(); // family name
        let name_span = self.current().span;
        let name = self.intern_span(name_span);
        self.bump(); // declaration name
        let params = if self.at(TokenKind::LParen) {
            self.parse_params()
        } else {
            Vec::new()
        };
        let mut body = ConstructBody::default();
        self.parse_construct_body(&mut body, Some((family, family_span)));
        let span = Span::from_bounds(start.start, self.previous_end());
        Some(ConstructDecl {
            kind: ConstructKind::Backed {
                family,
                family_span,
                params,
            },
            name,
            name_span,
            fields: body.fields,
            methods: body.methods,
            deferred: body.deferred,
            span,
        })
    }

    /// Skips an `extends`/`requires` header clause, recording it as deferred.
    ///
    /// Inheritance does not execute yet, so a clause is refused rather than
    /// modeled. Everything from the clause keyword up to the body `{` is
    /// consumed so the body still parses.
    fn skip_construct_header_clauses(&mut self, deferred: &mut Vec<DeferredConstruct>) {
        while !self.at(TokenKind::LBrace) && !self.at_eof() {
            let label = match self.current_kind() {
                TokenKind::Extends => "`extends` inheritance",
                TokenKind::Identifier if self.text_of(self.current().span) == "requires" => {
                    "`requires` clause"
                }
                _ => break,
            };
            let clause_start = self.current().span;
            while !self.at(TokenKind::LBrace) && !self.at_eof() {
                // A `requires { ... }` block carries its own braces; consume them
                // balanced so the body brace that follows is the real one.
                if self.text_of(clause_start) == "requires" && self.at(TokenKind::LBrace) {
                    break;
                }
                self.bump();
                if self.at(TokenKind::LBrace) {
                    // `requires { ... }` — skip the block, then keep scanning for
                    // more clauses.
                    if label == "`requires` clause" {
                        self.skip_balanced(TokenKind::LBrace, TokenKind::RBrace);
                    }
                    break;
                }
            }
            let span = Span::from_bounds(clause_start.start, self.previous_end());
            deferred.push(DeferredConstruct { label, span });
        }
    }

    /// Parses the braced member body shared by both construct forms.
    fn parse_construct_body(
        &mut self,
        body: &mut ConstructBody,
        backing_family: Option<(Symbol, Span)>,
    ) {
        if !self.expect(TokenKind::LBrace) {
            return;
        }
        while !self.at(TokenKind::RBrace) && !self.at_eof() {
            let before = self.pos;
            while self.eat(TokenKind::Semicolon) {}
            if self.at(TokenKind::RBrace) || self.at_eof() {
                break;
            }
            self.parse_construct_member(body, backing_family);
            while self.eat(TokenKind::Semicolon) {}
            if self.pos == before {
                self.bump();
            }
        }
        self.expect(TokenKind::RBrace);
    }

    /// Parses one construct member.
    fn parse_construct_member(
        &mut self,
        body: &mut ConstructBody,
        backing_family: Option<(Symbol, Span)>,
    ) {
        match self.current_kind() {
            TokenKind::At => self.parse_annotated_construct_member(body),
            TokenKind::Let => self.parse_construct_let(body, false),
            TokenKind::Var => {
                let span = self.current().span;
                self.error(
                    span,
                    "KPAR058",
                    "a construct member is declared with `let`: a construct's fields \
                     are its construction inputs, not reassignable state",
                );
                self.parse_construct_let(body, true);
            }
            TokenKind::Function => {
                if let Some(function) = self.parse_function(false, Execution::Inherited, false) {
                    body.methods.push(ConstructMethod {
                        computed: false,
                        required: false,
                        function,
                    });
                }
            }
            // `requires { function f(…) -> T … }` — the *section* spelling of
            // `@Required function`. One block instead of one annotation per
            // member; the members it produces are the same, so a family may mix
            // the two freely and conformance sees no difference.
            TokenKind::Identifier
                if self.at_word("requires") && self.peek(1).kind == TokenKind::LBrace =>
            {
                self.parse_construct_requires_section(body);
            }
            // `body { child … }` — the SwiftUI-style shorthand for a computed
            // member whose result type is the declaration's construct family.
            // A backed declaration has that family in its header, so the shorthand
            // becomes the same zero-argument computed method as an explicit
            // `let body: Widget { … }` member.
            TokenKind::Identifier if self.peek(1).kind == TokenKind::LBrace => {
                let start = self.current().span;
                let name_span = start;
                let name = self.intern_span(name_span);
                self.bump(); // member name
                let block = self.parse_block();
                let span = Span::from_bounds(start.start, self.previous_end());
                match backing_family {
                    Some((family, family_span)) => {
                        self.make_block_return_its_tail(&block);
                        // The family decides what this member returns, and the
                        // parser has no families — so the type ref defers the
                        // question rather than answering it with the family
                        // type, which is only ever right for a member the family
                        // never mentioned. See `TypeRef::ConstructMember`.
                        let ty = self.tree.add_type(TypeRef::ConstructMember {
                            family,
                            family_span,
                            member: name,
                            span: name_span,
                        });
                        body.methods.push(ConstructMethod {
                            computed: true,
                            required: false,
                            function: Self::computed_member_function(
                                name, name_span, ty, block, span,
                            ),
                        });
                    }
                    None => body.deferred.push(DeferredConstruct {
                        label: "a `body` shorthand on a construct family template",
                        span,
                    }),
                }
            }
            kind => {
                let span = self.current().span;
                self.error(
                    span,
                    "KPAR059",
                    format!(
                        "expected `let`, `function`, or an annotated member to start a \
                         construct member, found {}",
                        kind.describe()
                    ),
                );
                // Resynchronize to the next member rather than reporting every
                // stray token: skip — over balanced braces — to the next member
                // keyword or the body's close, so one malformed member is one
                // diagnostic, not a token-by-token cascade.
                self.recover_to_next_construct_member();
            }
        }
    }

    /// Skips to the next construct member or the body's closing `}`.
    ///
    /// Consumes a nested `{...}` balanced so a brace inside skipped content does
    /// not end the scan early, and stops at the first token that can begin a
    /// member so a real member after the damage still parses.
    fn recover_to_next_construct_member(&mut self) {
        // Step over the offending token first, so a member-start keyword sitting
        // at the cursor does not stall the scan on itself.
        if !self.at(TokenKind::RBrace) && !self.at_eof() {
            if self.at(TokenKind::LBrace) {
                self.skip_balanced(TokenKind::LBrace, TokenKind::RBrace);
            } else {
                self.bump();
            }
        }
        while !self.at(TokenKind::RBrace) && !self.at_eof() {
            match self.current_kind() {
                TokenKind::Let | TokenKind::Var | TokenKind::Function | TokenKind::At => break,
                TokenKind::LBrace => self.skip_balanced(TokenKind::LBrace, TokenKind::RBrace),
                _ => {
                    self.bump();
                }
            }
        }
    }
}
