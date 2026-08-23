//! Statement parsing for the recursive-descent parser.
//!
//! Recovery boundary: a statement that cannot be parsed becomes a
//! [`Stmt::Error`] and the cursor resynchronizes at the next `;`, `}`, or
//! statement-starting keyword, so one bad statement never derails the block.
//!
//! The multi-arm branch statement, `match`, lives in
//! [`branches`] — they are long enough together to crowd everything else, and
//! they share the struct-literal suppression rule that makes `subject {` open a
//! body rather than a literal.

use kira_core::Symbol;
use kira_source::Span;
use kira_syntax_model::TokenKind;
use kira_syntax_model::ast::{Block, ForIterable, Stmt, StmtId};
use kira_syntax_model::ownership::OwnershipMode;

use crate::Parser;

mod branches;

impl Parser<'_> {
    /// Parses one statement, returning its arena handle, or `None` when the
    /// position was consumed as pure recovery with no node produced.
    pub(crate) fn parse_stmt(&mut self) -> Option<StmtId> {
        match self.current_kind() {
            TokenKind::Let => Some(self.parse_let(false)),
            TokenKind::Var => Some(self.parse_let(true)),
            TokenKind::Return => Some(self.parse_return()),
            TokenKind::If => Some(self.parse_if()),
            TokenKind::While => Some(self.parse_while()),
            TokenKind::For => Some(self.parse_for()),
            TokenKind::Break => Some(self.parse_break()),
            TokenKind::Continue => Some(self.parse_continue()),
            TokenKind::Match => Some(self.parse_match()),
            TokenKind::Attempt => Some(self.parse_attempt()),
            _ => Some(self.parse_expr_or_assign()),
        }
    }

    /// Parses an expression statement, turning it into an assignment when an
    /// `=` follows.
    ///
    /// An assignment target is written with expression syntax (`p`, `p.x`,
    /// `b.size.x`), so it is parsed as one; deciding whether that expression
    /// actually names a place is semantics' job, not the parser's.
    fn parse_expr_or_assign(&mut self) -> StmtId {
        let start = self.current().span;
        let target = self.parse_expr();
        if !self.eat(TokenKind::Equals) {
            let span = Span::from_bounds(start.start, self.previous_end());
            return self.tree.add_stmt(Stmt::Expr { expr: target, span });
        }
        let value = self.parse_expr();
        let span = Span::from_bounds(start.start, self.previous_end());
        self.tree.add_stmt(Stmt::Assign {
            target,
            value,
            span,
        })
    }

    fn parse_let(&mut self, mutable: bool) -> StmtId {
        let start = self.current().span;
        self.bump(); // `let` / `var`
        let (name, name_span) = if self.at(TokenKind::Identifier) {
            let span = self.current().span;
            (self.intern_span(span), span)
        } else {
            self.error(self.current().span, "KPAR010", "expected a binding name");
            (Symbol::ERROR, self.current().span)
        };
        // Consumes the name just interned above.
        if self.at(TokenKind::Identifier) {
            self.bump();
        }
        // The same ownership prefix a parameter may carry, in the same place
        // relative to the type: `let f: borrow (Int) -> Void = g`. Parsed with
        // the shared helper so `let borrow = 1` — a binding *named* `borrow` —
        // keeps parsing as it always did.
        let (ownership, ownership_span, ty) = if self.eat(TokenKind::Colon) {
            let (ownership, ownership_span) = self.parse_ownership_prefix();
            (
                ownership,
                ownership_span,
                Some(self.parse_type_ref_statement_final()),
            )
        } else {
            (OwnershipMode::Owned, None, None)
        };
        let init = if self.eat(TokenKind::Equals) {
            self.parse_expr()
        } else {
            self.error(
                self.current().span,
                "KPAR011",
                "a binding needs an `=` initializer in the v0 subset",
            );
            self.error_expr(self.current().span)
        };
        let span = Span::from_bounds(start.start, self.previous_end());
        self.tree.add_stmt(Stmt::Let {
            name,
            name_span,
            mutable,
            ty,
            ownership,
            ownership_span,
            init,
            span,
        })
    }

    fn parse_return(&mut self) -> StmtId {
        let start = self.current().span;
        self.bump(); // `return`
        let value = if self.starts_expression() {
            Some(self.parse_expr())
        } else {
            None
        };
        let span = Span::from_bounds(start.start, self.previous_end());
        self.tree.add_stmt(Stmt::Return { value, span })
    }

    fn parse_if(&mut self) -> StmtId {
        let start = self.current().span;
        self.bump(); // `if`
        let cond = self.without_struct_literals(|parser| parser.parse_expr());
        let then_block = self.parse_block();
        let else_block = if self.eat(TokenKind::Else) {
            if self.at(TokenKind::If) {
                // `else if` desugars to an else-block holding one `if`.
                let nested = self.parse_if();
                let span = self.tree.stmt(nested).span();
                Some(Block {
                    stmts: vec![nested],
                    span,
                })
            } else {
                Some(self.parse_block())
            }
        } else {
            None
        };
        let span = Span::from_bounds(start.start, self.previous_end());
        self.tree.add_stmt(Stmt::If {
            cond,
            then_block,
            else_block,
            span,
        })
    }

    fn parse_while(&mut self) -> StmtId {
        let start = self.current().span;
        self.bump(); // `while`
        let cond = self.without_struct_literals(|parser| parser.parse_expr());
        let body = self.parse_block();
        let span = Span::from_bounds(start.start, self.previous_end());
        self.tree.add_stmt(Stmt::While { cond, body, span })
    }

    /// Parses `for <name> in <start>..<end> { … }` or `for <name> in <xs> { … }`.
    ///
    /// The `..` is what tells the two apart, and it can only be decided after
    /// the first expression is parsed — so the iterable is parsed first and
    /// classified second. There is no lookahead problem: an array iterable is
    /// an ordinary expression, and a range is one expression followed by `..`.
    ///
    /// The iterable sits between `in` and the body brace, so it is parsed with
    /// struct literals suppressed for the same reason a `while` condition is:
    /// otherwise `for x in xs {` reads the body as a struct literal.
    fn parse_for(&mut self) -> StmtId {
        let start = self.current().span;
        self.bump(); // `for`
        let (name, name_span) = if self.at(TokenKind::Identifier) {
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
        let iterable = self.without_struct_literals(|parser| {
            let first = parser.parse_expr();
            // `..` makes it a range; anything else makes the expression the
            // whole iterable. A bad iterable (`for i in 0 { }`) is not a parse
            // error any more — it parses as `Each { 0 }` and analysis reports
            // that an `Int` is not iterable, which is where the type is known.
            if parser.eat(TokenKind::DotDot) {
                ForIterable::Range {
                    start: first,
                    end: parser.parse_expr(),
                }
            } else {
                ForIterable::Each { array: first }
            }
        });
        let body = self.parse_block();
        let span = Span::from_bounds(start.start, self.previous_end());
        self.tree.add_stmt(Stmt::For {
            name,
            name_span,
            iterable,
            body,
            span,
        })
    }

    fn parse_break(&mut self) -> StmtId {
        let span = self.current().span;
        self.bump(); // `break`
        self.tree.add_stmt(Stmt::Break { span })
    }

    fn parse_continue(&mut self) -> StmtId {
        let span = self.current().span;
        self.bump(); // `continue`
        self.tree.add_stmt(Stmt::Continue { span })
    }

    /// Whether the current token can begin an expression (used to decide
    /// whether `return` carries a value).
    fn starts_expression(&self) -> bool {
        match self.current_kind() {
            TokenKind::IntLiteral
            | TokenKind::FloatLiteral
            | TokenKind::StringLiteral
            | TokenKind::True
            | TokenKind::False
            | TokenKind::Identifier
            | TokenKind::LParen
            // `return try g()` — `try` is a prefix operator in expression
            // position, so it starts an expression like `-` and `!` do.
            | TokenKind::Try
            | TokenKind::Minus
            | TokenKind::Bang
            // `return ~mask` — `~` is a prefix operator like `-` and `!`, so
            // it starts an expression the same way they do.
            | TokenKind::Tilde
            // `return [1, 2, 3]` — an array literal is a value like any
            // other, so `[` starts an expression.
            | TokenKind::LBracket => true,
            // `return .Red` — a leading-dot member is a value, so a `.` starts
            // an expression exactly when a variant name follows it, matching
            // the guard the primary parser reads the member with.
            TokenKind::Dot => self.peek(1).kind == TokenKind::Identifier,
            // `return { value in … }` — a closure is a value, so a `{` starts
            // an expression exactly when it opens one. Any other `{` does not:
            // a block is not an expression here.
            TokenKind::LBrace => self.at_closure_start(),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::parse;
    use kira_source::SourceId;
    use kira_syntax_model::ast::{Item, Stmt};

    /// The statements of the one function in `text`.
    fn body(text: &str) -> (kira_syntax_model::SyntaxTree, Vec<Stmt>) {
        let result = parse(SourceId::new(0), text);
        assert!(
            result.diagnostics.is_empty(),
            "unexpected diagnostics: {:?}",
            result.diagnostics
        );
        let function = match &result.tree.items()[0] {
            Item::Function(function) => function,
            other => panic!("expected a function, got {other:?}"),
        };
        let stmts = function
            .body
            .stmts
            .iter()
            .map(|&id| result.tree.stmt(id).clone())
            .collect();
        (result.tree.clone(), stmts)
    }

    /// `return .Red` must read the leading-dot member as the return value, not
    /// as a bare `return` followed by a stray `.Red` statement.
    #[test]
    fn a_return_takes_a_leading_dot_member_as_its_value() {
        let (tree, stmts) = body("function f() { return .Red }");
        assert_eq!(stmts.len(), 1, "expected a single `return` statement");
        match &stmts[0] {
            Stmt::Return {
                value: Some(expr), ..
            } => assert!(matches!(
                tree.expr(*expr),
                kira_syntax_model::ast::Expr::DotMember { .. }
            )),
            other => panic!("expected `return` with a value, got {other:?}"),
        }
    }

    #[test]
    fn a_for_loop_parses_its_variable_and_both_bounds() {
        let (_, stmts) = body("function f() { for i in 0..5 { } }");
        match &stmts[0] {
            Stmt::For { body, iterable, .. } => {
                assert!(body.stmts.is_empty());
                assert!(matches!(
                    iterable,
                    kira_syntax_model::ast::ForIterable::Range { .. }
                ));
            }
            other => panic!("expected a `for`, got {other:?}"),
        }
    }

    /// The `..` is the only thing separating the two `for` forms, and it is
    /// decided after the first expression rather than by lookahead.
    #[test]
    fn a_for_without_a_range_iterates_the_expression() {
        let (_, stmts) = body("function f() { for x in xs { } }");
        match &stmts[0] {
            Stmt::For { iterable, .. } => assert!(matches!(
                iterable,
                kira_syntax_model::ast::ForIterable::Each { .. }
            )),
            other => panic!("expected a `for`, got {other:?}"),
        }
    }

    /// The body brace must read as a block, not as a struct literal on the
    /// upper bound — the same ambiguity a `while` condition has.
    #[test]
    fn a_for_bound_does_not_swallow_the_body_brace() {
        let (tree, stmts) = body("function f() { for i in 0..n { let x = 1 } }");
        match &stmts[0] {
            Stmt::For { iterable, body, .. } => {
                assert_eq!(body.stmts.len(), 1, "the brace is the loop body");
                let kira_syntax_model::ast::ForIterable::Range { end, .. } = iterable else {
                    panic!("expected a range, got {iterable:?}");
                };
                assert!(
                    matches!(tree.expr(*end), kira_syntax_model::ast::Expr::Name { .. }),
                    "the upper bound is the bare name, not a literal"
                );
            }
            other => panic!("expected a `for`, got {other:?}"),
        }
    }

    /// The same ambiguity, on the array form: `for x in xs {` must read the
    /// brace as the body, not as an `xs` struct literal.
    #[test]
    fn a_for_array_iterable_does_not_swallow_the_body_brace() {
        let (tree, stmts) = body("function f() { for x in xs { let y = 1 } }");
        match &stmts[0] {
            Stmt::For { iterable, body, .. } => {
                assert_eq!(body.stmts.len(), 1, "the brace is the loop body");
                let kira_syntax_model::ast::ForIterable::Each { array } = iterable else {
                    panic!("expected an array iterable, got {iterable:?}");
                };
                assert!(
                    matches!(tree.expr(*array), kira_syntax_model::ast::Expr::Name { .. }),
                    "the iterable is the bare name, not a literal"
                );
            }
            other => panic!("expected a `for`, got {other:?}"),
        }
    }

    #[test]
    fn break_and_continue_parse_as_statements() {
        let (_, stmts) = body("function f() { while true { break } }");
        match &stmts[0] {
            Stmt::While { body, .. } => assert_eq!(body.stmts.len(), 1),
            other => panic!("expected a `while`, got {other:?}"),
        }
        let (_, stmts) = body("function f() { while true { continue } }");
        match &stmts[0] {
            Stmt::While { body, .. } => assert_eq!(body.stmts.len(), 1),
            other => panic!("expected a `while`, got {other:?}"),
        }
    }

    /// `for x in xs` parses as an `Each` without a range token. Reporting that
    /// an `Int` is not iterable belongs to analysis, where the type is known.
    #[test]
    fn a_non_iterable_for_is_left_for_analysis_to_report() {
        let result = parse(SourceId::new(0), "function f() { for i in 0 { } }");
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    }

    /// Recovery: a broken `for` header still leaves a parseable program.
    #[test]
    fn a_for_without_a_loop_variable_still_parses_the_program() {
        let result = parse(SourceId::new(0), "function f() { for in 0..5 { } }");
        assert!(!result.diagnostics.is_empty());
        assert_eq!(result.tree.items().len(), 1, "the function is still there");
    }
}
