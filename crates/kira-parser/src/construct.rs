//! Construct declaration-family parsing: the family template
//! `construct Family { ... }` and the construct-backed declaration
//! `Family Name(params) { ... }` that conforms to one.
//!
//! Both share a member body — stored `let` members, computed block-bodied
//! members (`let node: Any { expr }`), and `function` members — so they share a
//! parser. A family adds nothing to the header beyond its name; a backed
//! declaration adds a function-style parameter list, which becomes its
//! construction inputs.
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
    Block, ConstructDecl, ConstructField, ConstructKind, ConstructMethod, DeferredConstruct,
    Function, Stmt, TypeRef, TypeRefId,
};

use crate::Parser;

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
                        function,
                    });
                }
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
                        let ty = self.tree.add_type(TypeRef::Named {
                            name: family,
                            span: family_span,
                        });
                        body.methods.push(ConstructMethod {
                            computed: true,
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

    /// Parses an `@Name`-annotated construct member.
    fn parse_annotated_construct_member(&mut self, body: &mut ConstructBody) {
        let at_span = self.current().span;
        self.bump(); // `@`
        if !self.at(TokenKind::Identifier) {
            self.error(
                self.current().span,
                "KPAR003",
                "expected an annotation name after `@`",
            );
            return;
        }
        let name_span = self.current().span;
        let name = self.text_of(name_span).to_owned();
        self.bump();
        match name.as_str() {
            // `@Required let name: Any` — a member every backed declaration must
            // provide. This one executes: it is a stored field.
            "Required" => {
                if self.at(TokenKind::Let) {
                    self.parse_construct_let_required(body);
                } else {
                    self.error(
                        self.current().span,
                        "KPAR060",
                        "`@Required` annotates a `let` member",
                    );
                }
            }
            // `@Content let x: Any` — the compat spelling of a `some Any` child
            // slot. It parses to a real slot field: analysis fills it from a
            // construction's trailing children, and executes it when its declared
            // type is concrete.
            "Content" => {
                if self.at(TokenKind::Let) {
                    self.parse_construct_content_slot(body);
                } else {
                    self.error(
                        self.current().span,
                        "KPAR063",
                        "`@Content` annotates a `let` child-slot member",
                    );
                }
            }
            "Consuming" => {
                if self.at(TokenKind::Function) {
                    if let Some(function) = self.parse_function(false, Execution::Inherited, false)
                    {
                        body.methods.push(ConstructMethod {
                            computed: false,
                            function,
                        });
                    }
                } else {
                    self.error(
                        self.current().span,
                        "KPAR064",
                        "`@Consuming` annotates a `function` member",
                    );
                    self.consume_deferred_member(body, at_span, "malformed `@Consuming` member");
                }
            }
            other => {
                self.error(
                    name_span,
                    "KPAR061",
                    format!("`@{other}` is not a construct member annotation"),
                );
                self.consume_deferred_member(body, at_span, "unknown annotated member");
            }
        }
    }

    /// Consumes a not-yet-executable member (a `let` field or a `function`) so
    /// the rest of the body still parses, recording it as deferred.
    fn consume_deferred_member(
        &mut self,
        body: &mut ConstructBody,
        start: Span,
        label: &'static str,
    ) {
        match self.current_kind() {
            TokenKind::Let | TokenKind::Var => {
                let mut discard = ConstructBody::default();
                self.parse_construct_let(&mut discard, self.at(TokenKind::Var));
            }
            TokenKind::Function => {
                self.parse_function(false, Execution::Inherited, false);
            }
            _ => {
                // Nothing recognizable follows; step over one token so the loop
                // makes progress.
                self.bump();
            }
        }
        let span = Span::from_bounds(start.start, self.previous_end());
        body.deferred.push(DeferredConstruct { label, span });
    }

    /// Parses `@Required let name: Any [= default]`, with `let` at the cursor.
    fn parse_construct_let_required(&mut self, body: &mut ConstructBody) {
        let start = self.current().span;
        self.bump(); // `let`
        let Some((name, name_span, ty, slot)) = self.parse_construct_member_head() else {
            return;
        };
        if self.at(TokenKind::LBrace) {
            self.error(
                self.current().span,
                "KPAR062",
                "a `@Required` member has no computed body: it is a value the \
                 backed declaration supplies",
            );
            self.parse_block();
        }
        let default = self.eat(TokenKind::Equals).then(|| self.parse_expr());
        let span = Span::from_bounds(start.start, self.previous_end());
        body.fields.push(ConstructField {
            name,
            name_span,
            required: true,
            slot,
            ty,
            default,
            span,
        });
    }

    /// Parses `@Content let name: Any`, with `let` at the cursor — a child slot in
    /// its compat spelling. Equivalent to a `let name: some Any` field.
    fn parse_construct_content_slot(&mut self, body: &mut ConstructBody) {
        let start = self.current().span;
        self.bump(); // `let`
        let Some((name, name_span, ty, _slot)) = self.parse_construct_member_head() else {
            return;
        };
        let default = self.eat(TokenKind::Equals).then(|| self.parse_expr());
        let span = Span::from_bounds(start.start, self.previous_end());
        body.fields.push(ConstructField {
            name,
            name_span,
            required: false,
            slot: true,
            ty,
            default,
            span,
        });
    }

    /// Parses a plain or computed `let` construct member, with `let`/`var` at
    /// the cursor.
    fn parse_construct_let(&mut self, body: &mut ConstructBody, _is_var: bool) {
        let start = self.current().span;
        self.bump(); // `let` / `var`
        let Some((name, name_span, ty, slot)) = self.parse_construct_member_head() else {
            return;
        };
        if self.at(TokenKind::LBrace) {
            // `let node: Any { block }` — a computed member: a zero-argument method
            // read as a property.
            let block = self.parse_block();
            self.make_block_return_its_tail(&block);
            let span = Span::from_bounds(start.start, self.previous_end());
            body.methods.push(ConstructMethod {
                computed: true,
                function: Self::computed_member_function(name, name_span, ty, block, span),
            });
            return;
        }
        let default = self.eat(TokenKind::Equals).then(|| self.parse_expr());
        let span = Span::from_bounds(start.start, self.previous_end());
        body.fields.push(ConstructField {
            name,
            name_span,
            required: false,
            slot,
            ty,
            default,
            span,
        });
    }

    /// Parses the `name: Type` head shared by every `let` construct member,
    /// returning whether the type was written as a child slot (`some X` /
    /// `[some X]`).
    fn parse_construct_member_head(&mut self) -> Option<(Symbol, Span, TypeRefId, bool)> {
        if !self.at(TokenKind::Identifier) {
            self.error(self.current().span, "KPAR010", "expected a member name");
            return None;
        }
        let name_span = self.current().span;
        let name = self.intern_span(name_span);
        self.bump();
        self.expect(TokenKind::Colon);
        let (ty, slot) = self.parse_construct_field_type();
        Some((name, name_span, ty, slot))
    }

    /// Parses a construct field's type, recognizing the child-slot spellings
    /// `some X` and `[some X]`.
    ///
    /// A slot's stored type is its *element* type: `some X` yields `X` and
    /// `[some X]` yields `[X]`, so a single slot and a list slot are told apart
    /// downstream by whether the type is an array. A plain type is not a slot.
    fn parse_construct_field_type(&mut self) -> (TypeRefId, bool) {
        // `some X`
        if self.at_word("some") {
            self.bump(); // `some`
            return (self.parse_type_ref(), true);
        }
        // `[some X]`
        if self.at(TokenKind::LBracket)
            && self.peek(1).kind == TokenKind::Identifier
            && self.text_of(self.peek(1).span) == "some"
        {
            let start = self.current().span;
            self.bump(); // `[`
            self.bump(); // `some`
            let element = self.parse_type_ref();
            self.expect(TokenKind::RBracket);
            let span = Span::from_bounds(start.start, self.previous_end());
            return (self.tree.add_type(TypeRef::Array { element, span }), true);
        }
        (self.parse_type_ref(), false)
    }

    /// Rewrites a block's trailing expression statement into a `return`, in
    /// place.
    ///
    /// A computed member is an expression bridge — `let node: Node { body.node }`
    /// yields its final expression — so its block returns its tail, the way the
    /// source reads it. Statements live in the tree's arena, so the rewrite
    /// replaces the arena entry the block's last id names.
    fn make_block_return_its_tail(&mut self, body: &Block) {
        let Some(&last) = body.stmts.last() else {
            return;
        };
        if let Stmt::Expr { expr, span } = self.tree.stmt(last) {
            let (expr, span) = (*expr, *span);
            self.tree.stmts[last] = Stmt::Return {
                value: Some(expr),
                span,
            };
        }
    }

    /// Builds the zero-argument method a computed member (`let node: Any { … }`)
    /// desugars to: no parameters, result type `Any`, body the written block.
    fn computed_member_function(
        name: Symbol,
        name_span: Span,
        ty: TypeRefId,
        body: Block,
        span: Span,
    ) -> Function {
        Function {
            name,
            name_span,
            is_main: false,
            foreign: None,
            export: None,
            execution: Execution::Inherited,
            params: Vec::new(),
            return_type: Some(ty),
            body,
            span,
        }
    }
}
