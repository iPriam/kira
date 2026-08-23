//! Parsing a construction's trailing content block and its items.
//!
//! A content block is the braced run of children after a construction —
//! `HStack { Text("a") For(x in xs) { … } }`. Its items are bare child
//! expressions and the `For`/`if` builders that produce children by control
//! flow; the builders are recognized only here, so a `For(...)` or `if`
//! anywhere else parses as it always did. Split out of [`super`] on the
//! file-size ladder so the precedence-climbing core stays about ordinary
//! expressions.

use kira_core::Symbol;
use kira_source::Span;
use kira_syntax_model::TokenKind;
use kira_syntax_model::ast::{CallArg, Expr, ExprId, TrailingClosure};

use crate::Parser;

/// A `{ … }` with no `in`, in both the readings it may turn out to be.
pub(crate) struct DeferredBrace {
    /// The brace read as a construction's content items.
    pub(crate) children: Vec<ExprId>,
    /// The brace read as a zero-parameter closure, when it reads that way.
    pub(crate) closure: Option<TrailingClosure>,
}

impl Parser<'_> {
    /// Whether the cursor sits on a `{` that opens a **content block** — a
    /// braced run of bare child expressions, as in `HStack { Text("a") }`.
    ///
    /// Pure lookahead. A content block is what a `{` is when it is neither a
    /// closure (`{ x in … }`) nor a struct literal (whose first field is
    /// `name =` / `name :`) nor one of the empty/`let` blocks handled as a bare
    /// construction.
    pub(crate) fn at_content_block(&self) -> bool {
        if !self.at(TokenKind::LBrace) || self.no_struct_literal || self.at_closure_start() {
            return false;
        }
        // `{}` and `{ field = … }` / `{ field : … }` are struct literals.
        if self.peek(1).kind == TokenKind::RBrace {
            return false;
        }
        if (self.peek(1).kind == TokenKind::Identifier
            && matches!(self.peek(2).kind, TokenKind::Equals | TokenKind::Colon))
            || self.at_braced_field_override()
        {
            return false;
        }
        true
    }

    /// Whether a bare name is followed by the construct-only brace forms that
    /// are ambiguous with a struct literal: empty braces or `let` overrides.
    ///
    /// The parser records these as calls and leaves the final choice between a
    /// construct update and a plain empty struct to semantic analysis, where
    /// local bindings and construct declarations are visible.
    pub(crate) fn at_bare_construct_block(&self) -> bool {
        if !self.at(TokenKind::LBrace) || self.no_struct_literal || self.at_closure_start() {
            return false;
        }
        self.peek(1).kind == TokenKind::RBrace
            || self.peek(1).kind == TokenKind::Let
            || self.at_braced_field_override()
    }

    /// Whether the first item after `{` is the canonical dotted construction
    /// override path. Plain `field: value` remains a struct literal; the dot
    /// is what makes this the otherwise ambiguous construct-update spelling.
    fn at_braced_field_override(&self) -> bool {
        if self.peek(1).kind != TokenKind::Identifier {
            return false;
        }
        let mut index = 2;
        let mut dotted = false;
        while self.peek(index).kind == TokenKind::Dot
            && self.peek(index + 1).kind == TokenKind::Identifier
        {
            dotted = true;
            index += 2;
        }
        dotted && matches!(self.peek(index).kind, TokenKind::Equals | TokenKind::Colon)
    }

    /// Parses `{ child child … }`, with the cursor on `{`, into the list of
    /// child expressions it holds.
    ///
    /// Children are separated by nothing at all — newlines are insignificant in
    /// Kira, so a run of constructions one per line is the common shape — the
    /// same way an array literal's elements are.
    pub(crate) fn parse_content_block(&mut self) -> Vec<ExprId> {
        self.bump(); // `{`
        let mut children = Vec::new();
        self.with_struct_literals(|parser| {
            while !parser.at(TokenKind::RBrace) && !parser.at_eof() {
                let before = parser.pos;
                while parser.eat(TokenKind::Semicolon) {}
                if parser.at(TokenKind::RBrace) || parser.at_eof() {
                    break;
                }
                children.push(parser.parse_content_item());
                parser.eat(TokenKind::Comma);
                if parser.pos == before {
                    parser.bump();
                }
            }
        });
        self.expect(TokenKind::RBrace);
        children
    }

    /// Whether the cursor sits on a `{` that closes a construction.
    ///
    /// Looser than [`Self::at_content_block`] on purpose. An empty `{ }` and a
    /// `{ let field = value }` override block belong to the construction after
    /// a call, where a struct literal is no longer possible.
    pub(crate) fn at_trailing_block(&self) -> bool {
        self.at(TokenKind::LBrace) && !self.no_struct_literal && !self.at_closure_start()
    }

    /// Parses the `{ … }` at the cursor both ways — as a construction's content
    /// and as a zero-parameter closure body — and keeps the readings that hold.
    ///
    /// A `{` with no `in` means one of two things, and which one depends on the
    /// callee: `HStack { Text("a") }` passes children, `doThing { print("a") }`
    /// passes a closure. The parser does not know the callee, so it stops
    /// guessing and carries both; analysis, which holds the signature, takes
    /// the one the parameter asks for.
    ///
    /// A reading that does not parse is dropped, and its diagnostics with it.
    /// Only when *neither* reads cleanly do the content reading's diagnostics
    /// stand, because content is what a brace after a name means when nothing
    /// else fits.
    pub(crate) fn parse_deferred_brace(&mut self, args: &mut Vec<CallArg>) -> DeferredBrace {
        let start = self.pos;
        let mark = self.diagnostics.len();
        let brace = self.current().span;

        let block = self.parse_block();
        let block_reads = self.diagnostics.len() == mark;
        self.diagnostics.truncate(mark);
        let after_block = self.pos;

        self.pos = start;
        let mut content_args = Vec::new();
        let children = self.parse_trailing_block(&mut content_args);
        let content_reads = self.diagnostics.len() == mark;
        let after_content = self.pos;

        let closure = block_reads.then(|| {
            let span = Span::from_bounds(brace.start, self.previous_end());
            TrailingClosure {
                closure: self.tree.add_expr(Expr::Closure {
                    params: Vec::new(),
                    body: block,
                    span,
                }),
                // The brace sits after every parenthesized argument and before
                // any named fill written after it, which is exactly here.
                slot: args.len() as u32,
                content_args: content_args.len() as u32,
            }
        });
        // Only the closure reads: the content reading was never what this is,
        // so neither its nodes nor its diagnostics belong to the program.
        if block_reads && !content_reads {
            self.diagnostics.truncate(mark);
            self.pos = after_block;
            return DeferredBrace {
                children: Vec::new(),
                closure: closure.map(|mut trailing| {
                    trailing.content_args = 0;
                    trailing
                }),
            };
        }
        self.pos = after_content;
        args.extend(content_args);
        DeferredBrace { children, closure }
    }

    /// Parses the braced block that closes a construction, returning its
    /// children and appending its `let field = value` overrides to `args`.
    ///
    /// One block holds both because the language spells them the same way. An
    /// override is a field initializer written after the argument list rather
    /// than inside it, so it becomes an ordinary labeled argument and every
    /// later check — unknown label, duplicate label, missing argument — reads it
    /// the way it reads one written between the parentheses.
    pub(crate) fn parse_trailing_block(&mut self, args: &mut Vec<CallArg>) -> Vec<ExprId> {
        self.bump(); // `{`
        let mut children = Vec::new();
        self.with_struct_literals(|parser| {
            while !parser.at(TokenKind::RBrace) && !parser.at_eof() {
                let before = parser.pos;
                while parser.eat(TokenKind::Semicolon) {}
                if parser.at(TokenKind::RBrace) || parser.at_eof() {
                    break;
                }
                match parser.parse_override() {
                    Some(argument) => args.push(argument),
                    None => children.push(parser.parse_content_item()),
                }
                parser.eat(TokenKind::Comma);
                if parser.pos == before {
                    parser.bump();
                }
            }
        });
        self.expect(TokenKind::RBrace);
        children
    }

    /// Whether the cursor sits on a **named child fill**: the `detail:` of
    /// `NavigationSplitView { … } detail: { … }`.
    ///
    /// A fill binds to the construction it is written *after*; an override
    /// binds to the construction it is written *inside*. When both readings fit
    /// — a fill written directly after a child that closed its own block — the
    /// nearer construction wins, and `let name = value` remains the spelling
    /// that names the enclosing one unambiguously.
    pub(crate) fn at_named_fill(&self) -> bool {
        !self.no_named_fill
            && !self.no_struct_literal
            && self.at(TokenKind::Identifier)
            && self.peek(1).kind == TokenKind::Colon
    }

    /// Whether the construction just parsed closed with a `}` of its own, which
    /// is what admits the named fills that follow it.
    ///
    /// Without this, a bare `Text("a")` inside a content block would swallow the
    /// `spacing: 8` override written after it, which belongs to the enclosing
    /// construction instead.
    pub(crate) fn at_fillable_construction(&self, base: ExprId) -> bool {
        self.previous_kind() == TokenKind::RBrace
            && matches!(self.tree.expr(base), Expr::Call { .. })
    }

    /// Parses the run of named child fills closing a construction, folding each
    /// into the call's arguments; analysis routes a fill whose label names a
    /// child slot to that slot.
    pub(super) fn attach_named_fills(&mut self, mut base: ExprId) -> ExprId {
        while self.at_named_fill() {
            let Expr::Call {
                callee,
                callee_span,
                braced,
                type_args,
                args,
                children,
                trailing_closure,
                span: base_span,
            } = self.tree.expr(base).clone()
            else {
                break;
            };
            let mut args = args;
            args.push(self.parse_named_fill());
            let span = Span::from_bounds(base_span.start, self.previous_end());
            base = self.tree.add_expr(Expr::Call {
                callee,
                callee_span,
                braced,
                type_args,
                args,
                children,
                trailing_closure,
                span,
            });
        }
        base
    }

    /// Parses one `name: <fill>` with the cursor on the name.
    fn parse_named_fill(&mut self) -> CallArg {
        let label_span = self.current().span;
        let label = self.intern_span(label_span);
        self.bump(); // name
        self.bump(); // `:`
        let value = self.parse_fill_value();
        CallArg {
            label: Some(label),
            label_span: Some(label_span),
            value,
            span: Span::from_bounds(label_span.start, self.previous_end()),
        }
    }

    /// Parses what a named fill or a construction override binds to.
    ///
    /// A bare `{ … }` is a content block rather than a value — the anonymous
    /// form that fills a child slot with the children it holds. Anything else
    /// is an ordinary expression, which covers both a narrowing construction
    /// (`detail: DetailView { … }`) and a plain value (`detail: view`).
    fn parse_fill_value(&mut self) -> ExprId {
        if self.at(TokenKind::LBrace) && !self.at_closure_start() {
            let start = self.current().span;
            let mut discard = Vec::new();
            let brace =
                self.with_struct_literals(|parser| parser.parse_deferred_brace(&mut discard));
            let span = Span::from_bounds(start.start, self.previous_end());
            return self.tree.add_expr(Expr::Content {
                children: brace.children,
                // A named fill *is* the argument, so where it sits among the
                // others is already decided; only the reading is open.
                closure: brace.closure.map(|trailing| trailing.closure),
                span,
            });
        }
        self.without_named_fills(|parser| parser.parse_expr())
    }

    /// Parses `let field[.nested] = value` or the canonical
    /// `field.nested: value` spelling as a labeled argument, or nothing.
    ///
    /// `None` leaves the cursor untouched, so the caller reads the item as a
    /// child instead.
    fn parse_override(&mut self) -> Option<CallArg> {
        let has_let = self.at(TokenKind::Let);
        let starts_bare_path = self.at(TokenKind::Identifier)
            && matches!(self.peek(1).kind, TokenKind::Dot | TokenKind::Colon);
        if (!has_let && !starts_bare_path)
            || (has_let && self.peek(1).kind != TokenKind::Identifier)
        {
            return None;
        }
        if has_let && self.peek(2).kind != TokenKind::Equals && self.peek(2).kind != TokenKind::Dot
        {
            return None;
        }
        let start = self.current().span.start;
        if has_let {
            self.bump(); // `let`
        }
        let first_span = self.current().span;
        let mut path = self.text_of(first_span).to_owned();
        let mut label_span = first_span;
        self.bump(); // name
        while self.eat(TokenKind::Dot) {
            if !self.at(TokenKind::Identifier) {
                self.error(
                    self.current().span,
                    "KPAR022",
                    "expected a field name after `.` in a construction override",
                );
                break;
            }
            let segment_span = self.current().span;
            path.push('.');
            path.push_str(self.text_of(segment_span));
            label_span = Span::from_bounds(label_span.start, segment_span.end());
            self.bump();
        }
        let label = self.intern_text(&path, label_span);
        if !self.eat(TokenKind::Colon) {
            self.expect(TokenKind::Equals);
        }
        let value = self.parse_fill_value();
        Some(CallArg {
            label: Some(label),
            label_span: Some(label_span),
            value,
            span: Span::from_bounds(start, self.previous_end()),
        })
    }

    /// Parses one item of a content block: a `For`/`if` builder, or a bare
    /// child expression.
    ///
    /// A builder produces children by control flow — `For(x in xs) { … }`
    /// contributes one child per iteration, `if cond { … } else { … }`
    /// contributes the taken branch's children. Everything else is an ordinary
    /// child expression. The builders are recognized only here, so a `For(...)`
    /// or `if` anywhere but a content block is parsed as it always was.
    fn parse_content_item(&mut self) -> ExprId {
        if self.at_word("For") && self.peek(1).kind == TokenKind::LParen {
            return self.parse_content_for();
        }
        if self.at(TokenKind::If) {
            return self.parse_content_if();
        }
        self.parse_expr()
    }

    /// Parses `For(<binding> in <iterable>) { <items> }` into an
    /// [`Expr::ContentFor`], with the cursor on `For`.
    fn parse_content_for(&mut self) -> ExprId {
        let start = self.current().span;
        self.bump(); // `For`
        self.expect(TokenKind::LParen);
        let (binding, binding_span) = if self.at(TokenKind::Identifier) {
            let span = self.current().span;
            (self.intern_span(span), span)
        } else {
            self.error(self.current().span, "KPAR012", "expected a loop variable");
            (Symbol::ERROR, self.current().span)
        };
        // Consumes the loop variable just interned above.
        if self.at(TokenKind::Identifier) {
            self.bump();
        }
        self.expect(TokenKind::In);
        let iterable = self.parse_expr();
        self.expect(TokenKind::RParen);
        let body = if self.at(TokenKind::LBrace) {
            self.parse_content_block()
        } else {
            self.error(
                self.current().span,
                "KPAR065",
                "expected `{` to open a `For` builder body",
            );
            Vec::new()
        };
        let span = Span::from_bounds(start.start, self.previous_end());
        self.tree.add_expr(Expr::ContentFor {
            binding,
            binding_span,
            iterable,
            body,
            span,
        })
    }

    /// Parses `if <cond> { <items> } [else if …] [else { <items> }]` into an
    /// [`Expr::ContentIf`], with the cursor on `if`. An `else if` chain nests as
    /// a single-item `else` branch holding another content `if`.
    fn parse_content_if(&mut self) -> ExprId {
        let start = self.current().span;
        self.bump(); // `if`
        let cond = self.without_struct_literals(|parser| parser.parse_expr());
        let then_body = self.parse_content_block();
        let else_body = if self.eat(TokenKind::Else) {
            if self.at(TokenKind::If) {
                vec![self.parse_content_if()]
            } else {
                self.parse_content_block()
            }
        } else {
            Vec::new()
        };
        let span = Span::from_bounds(start.start, self.previous_end());
        self.tree.add_expr(Expr::ContentIf {
            cond,
            then_body,
            else_body,
            span,
        })
    }

    /// Attaches a trailing content block's children to a construction call, or
    /// returns `None` when the base is not a call the children can attach to.
    pub(super) fn attach_content_block(&mut self, base: ExprId) -> Option<ExprId> {
        // A modifier takes content too. `.toolbar { … }` is a method call with
        // children, and it is the shape most of SwiftUI's content-bearing
        // modifiers have — an overlay, a context menu, a sheet. Analysis decides
        // whether the method actually has a slot for them; the parser's job is
        // only to stop dropping the brace on the floor.
        if let Expr::MethodCall {
            receiver,
            method,
            method_span,
            args,
            children: existing,
            span: base_span,
        } = self.tree.expr(base).clone()
        {
            if !existing.is_empty() {
                return None;
            }
            // Only the content reading. A block a method means as a CLOSURE is
            // written with `in` and was taken by `attach_trailing_closure`
            // before the cursor ever reached here, so a brace arriving at this
            // point can only be content — and carrying the closure reading too
            // would let it win over the children the method actually asked for.
            let mut args = args;
            let brace = self.parse_deferred_brace(&mut args);
            let span = Span::from_bounds(base_span.start, self.previous_end());
            return Some(self.tree.add_expr(Expr::MethodCall {
                receiver,
                method,
                method_span,
                args,
                children: brace.children,
                span,
            }));
        }
        let Expr::Call {
            callee,
            callee_span,
            braced,
            type_args,
            args,
            children: existing,
            trailing_closure: existing_closure,
            span: base_span,
        } = self.tree.expr(base).clone()
        else {
            return None;
        };
        // A second content block on one construction is malformed; the first
        // already holds its children, so leave the base as it stands.
        if !existing.is_empty() || existing_closure.is_some() {
            return None;
        }
        let mut args = args;
        let brace = self.parse_deferred_brace(&mut args);
        let span = Span::from_bounds(base_span.start, self.previous_end());
        Some(self.tree.add_expr(Expr::Call {
            callee,
            callee_span,
            braced,
            type_args,
            args,
            children: brace.children,
            trailing_closure: brace.closure,
            span,
        }))
    }
}
