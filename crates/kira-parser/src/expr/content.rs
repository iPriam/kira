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
use kira_syntax_model::ast::{CallArg, Expr, ExprId};

use crate::Parser;

impl Parser<'_> {
    /// Whether the cursor sits on a `{` that opens a **content block** — a
    /// braced run of bare child expressions, as in `HStack { Text("a") }`.
    ///
    /// Pure lookahead. A content block is what a `{` is when it is neither a
    /// closure (`{ x in … }`) nor a struct literal (whose first field is
    /// `name =` / `name :`) nor empty (`{}`, which stays a struct literal). Only
    /// the leftover — a `{` whose first token begins an expression rather than a
    /// field binder — opens children.
    pub(crate) fn at_content_block(&self) -> bool {
        if !self.at(TokenKind::LBrace) || self.no_struct_literal || self.at_closure_start() {
            return false;
        }
        // `{}` and `{ field = … }` / `{ field : … }` are struct literals.
        if self.peek(1).kind == TokenKind::RBrace {
            return false;
        }
        if self.peek(1).kind == TokenKind::Identifier
            && matches!(self.peek(2).kind, TokenKind::Equals | TokenKind::Colon)
        {
            return false;
        }
        true
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
    /// Looser than [`Self::at_content_block`] on purpose, and only usable where
    /// the cursor has just left a call's `)`: there a `{` cannot be a struct
    /// literal, because the literal's own name was consumed by the call. So an
    /// empty `{ }` and a `{ let field = value }` override block — neither of
    /// which opens *children* — still belong to the construction here, where in
    /// bare-name position they would be a struct literal.
    pub(crate) fn at_trailing_block(&self) -> bool {
        self.at(TokenKind::LBrace) && !self.no_struct_literal && !self.at_closure_start()
    }

    /// Parses the braced block that closes a construction, returning its
    /// children and appending its `let field = value` overrides to `args`.
    ///
    /// One block holds both because the language spells them the same way. An
    /// override is a field initializer written after the argument list rather
    /// than inside it, so it becomes an ordinary labeled argument and every
    /// later check — unknown label, duplicate label, missing argument — reads it
    /// the way it reads one written between the parentheses.
    fn parse_trailing_block(&mut self, args: &mut Vec<CallArg>) -> Vec<ExprId> {
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

    /// Parses `let field = value` as a labeled argument, or nothing.
    ///
    /// `None` leaves the cursor untouched, so the caller reads the item as a
    /// child instead.
    fn parse_override(&mut self) -> Option<CallArg> {
        if !self.at(TokenKind::Let)
            || self.peek(1).kind != TokenKind::Identifier
            || self.peek(2).kind != TokenKind::Equals
        {
            return None;
        }
        let start = self.current().span.start;
        self.bump(); // `let`
        let label_span = self.current().span;
        let label = self.intern_span(label_span);
        self.bump(); // name
        self.bump(); // `=`
        let value = self.parse_expr();
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
        let Expr::Call {
            callee,
            callee_span,
            type_args,
            args,
            children: existing,
            span: base_span,
        } = self.tree.expr(base).clone()
        else {
            return None;
        };
        // A second content block on one construction is malformed; the first
        // already holds its children, so leave the base as it stands.
        if !existing.is_empty() {
            return None;
        }
        let mut args = args;
        let children = self.parse_trailing_block(&mut args);
        let span = Span::from_bounds(base_span.start, self.previous_end());
        Some(self.tree.add_expr(Expr::Call {
            callee,
            callee_span,
            type_args,
            args,
            children,
            span,
        }))
    }
}
