//! Closure literals and the trailing-callback form.
//!
//! A closure is `{ params in body }`. That opening `{` is the same token a
//! struct literal and a block open with, so the grammar is decided by a bounded
//! lookahead rather than by context: a `{` starts a closure exactly when what
//! follows is `in`, or a comma-separated run of identifiers followed by `in`.
//! Nothing else can look like that — a struct literal's first field is
//! `name =` or `name :`, and a block's first statement starts with a keyword,
//! a literal, or a name followed by something other than `in`.
//!
//! The same lookahead serves the trailing form: `app.onEvent { value in … }`
//! appends the closure as a final argument to the call it follows. That is
//! gated on struct literals being permitted, because both forms answer the same
//! question — whether a `{` after an expression belongs to the expression or
//! opens the block of an enclosing `if`/`while`/`for`/`switch`.

use kira_source::Span;
use kira_syntax_model::TokenKind;
use kira_syntax_model::ast::{ClosureParam, Expr, ExprId};

use crate::Parser;

impl Parser<'_> {
    /// Whether the cursor sits on a `{` that opens a closure literal.
    ///
    /// Pure lookahead: it consumes nothing, so a `false` leaves the cursor
    /// exactly where a struct literal or a block parse expects it.
    pub(crate) fn at_closure_brace(&self) -> bool {
        if !self.at(TokenKind::LBrace) {
            return false;
        }
        if self.peek(1).kind == TokenKind::In {
            return true;
        }
        let mut ahead = 1;
        while self.peek(ahead).kind == TokenKind::Identifier {
            ahead += 1;
            if self.peek(ahead).kind == TokenKind::Comma {
                ahead += 1;
                continue;
            }
            break;
        }
        // `ahead == 1` means the run was empty, so the `{` was never a closure.
        ahead > 1 && self.peek(ahead).kind == TokenKind::In
    }

    /// Whether the cursor sits on a `{` that was *meant* to open a closure but
    /// whose parameter list is malformed.
    ///
    /// A comma is the tell. `{ a, 1 in }` and `{ a, b }` cannot be a struct
    /// literal (its fields are `name =` or `name :`) and cannot be a block (a
    /// statement does not start `a ,`), so the only thing they can be is a
    /// closure header with a mistake in it. Recognizing that here is what lets
    /// the mistake be reported once, against the parameter, instead of
    /// cascading into "expected an expression" for every token after it.
    fn at_malformed_closure_brace(&self) -> bool {
        if !self.at(TokenKind::LBrace) || self.peek(1).kind != TokenKind::Identifier {
            return false;
        }
        let mut ahead = 1;
        let mut saw_comma = false;
        while self.peek(ahead).kind == TokenKind::Identifier {
            ahead += 1;
            if self.peek(ahead).kind == TokenKind::Comma {
                saw_comma = true;
                ahead += 1;
                continue;
            }
            break;
        }
        saw_comma && self.peek(ahead).kind != TokenKind::In
    }

    /// Whether a `{` here opens a closure, well-formed or not.
    pub(crate) fn at_closure_start(&self) -> bool {
        self.at_closure_brace() || self.at_malformed_closure_brace()
    }

    /// Parses `{ params in body }`, with the cursor on `{`.
    ///
    /// Only called when [`Parser::at_closure_start`] holds, so the `{` is known
    /// to open a closure — but not that it is well-formed: the malformed shape
    /// reaches here too, which is what turns one bad parameter into one
    /// diagnostic instead of a cascade.
    pub(crate) fn parse_closure(&mut self) -> ExprId {
        let start = self.current().span;
        self.bump(); // `{`
        let mut params = Vec::new();
        while !self.at(TokenKind::In) && !self.at(TokenKind::RBrace) && !self.at_eof() {
            if !self.at(TokenKind::Identifier) {
                let span = self.current().span;
                self.error(
                    span,
                    "KPAR039",
                    format!(
                        "expected a closure parameter name, found {}",
                        self.current_kind().describe()
                    ),
                );
                break;
            }
            let span = self.current().span;
            let name = self.intern_span(span);
            self.bump();
            params.push(ClosureParam { name, span });
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::In);
        // The body is a statement list, not a fresh block: the `{` was already
        // consumed, and its `}` closes the closure.
        let body = self.parse_block_body(start);
        let span = Span::from_bounds(start.start, self.previous_end());
        self.tree.add_expr(Expr::Closure { params, body, span })
    }

    /// Appends a trailing closure to `base` as the call's final argument.
    ///
    /// `f { … }` and `f(1) { … }` both mean a call whose last argument is the
    /// closure, so a bare name or field access is promoted to a call here and
    /// an existing call simply grows an argument. Promoting is what makes
    /// `graphics.run { frame in … }` a method call rather than a field read
    /// followed by a stray block.
    pub(crate) fn attach_trailing_closure(&mut self, base: ExprId) -> ExprId {
        let closure = self.parse_closure();
        let start = self.tree.expr(base).span().start;
        let span = Span::from_bounds(start, self.previous_end());
        match self.tree.expr(base).clone() {
            Expr::Call {
                callee,
                callee_span,
                mut args,
                ..
            } => {
                args.push(self.positional_arg(closure));
                self.tree.add_expr(Expr::Call {
                    callee,
                    callee_span,
                    args,
                    span,
                })
            }
            Expr::MethodCall {
                receiver,
                method,
                method_span,
                mut args,
                ..
            } => {
                args.push(self.positional_arg(closure));
                self.tree.add_expr(Expr::MethodCall {
                    receiver,
                    method,
                    method_span,
                    args,
                    span,
                })
            }
            Expr::Name { symbol, span: name } => self.tree.add_expr(Expr::Call {
                callee: symbol,
                callee_span: name,
                args: vec![self.positional_arg(closure)],
                span,
            }),
            Expr::Field {
                base: receiver,
                field,
                field_span,
                ..
            } => self.tree.add_expr(Expr::MethodCall {
                receiver,
                method: field,
                method_span: field_span,
                args: vec![self.positional_arg(closure)],
                span,
            }),
            // Anything else — a literal, an index, an operator result — is not
            // something a trailing closure can attach to. Reporting it here
            // keeps the closure parsed (so its body still contributes
            // diagnostics) instead of leaving a `{` the caller cannot use.
            _ => {
                self.error(
                    span,
                    "KPAR040",
                    "a trailing closure must follow a call, a name, or a method",
                );
                self.error_expr(span)
            }
        }
    }
}
