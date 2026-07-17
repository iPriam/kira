//! Statement parsing for the recursive-descent parser.
//!
//! Recovery boundary: a statement that cannot be parsed becomes a
//! [`Stmt::Error`] and the cursor resynchronizes at the next `;`, `}`, or
//! statement-starting keyword, so one bad statement never derails the block.

use kira_core::Symbol;
use kira_source::Span;
use kira_syntax_model::TokenKind;
use kira_syntax_model::ast::{Block, Stmt, StmtId};

use crate::Parser;

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
            TokenKind::Switch | TokenKind::Match => Some(self.parse_unsupported_stmt()),
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
        if self.at(TokenKind::Identifier) {
            self.bump();
        }
        let ty = if self.eat(TokenKind::Colon) {
            Some(self.parse_type_ref())
        } else {
            None
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

    /// Parses `for <name> in <start>..<end> { … }`.
    ///
    /// The bounds sit between `in` and the body brace, so they are parsed with
    /// struct literals suppressed for the same reason a `while` condition is:
    /// otherwise `for i in 0..n {` reads the body as a struct literal.
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
        if self.at(TokenKind::Identifier) {
            self.bump();
        }
        self.expect(TokenKind::In);
        let (range_start, range_end) = self.without_struct_literals(|parser| {
            let range_start = parser.parse_expr();
            if !parser.expect(TokenKind::DotDot) {
                // Without `..` there is no upper bound to parse; the lower one
                // stands alone and the range is poisoned rather than guessed.
                let span = parser.current().span;
                return (range_start, parser.error_expr(span));
            }
            (range_start, parser.parse_expr())
        });
        let body = self.parse_block();
        let span = Span::from_bounds(start.start, self.previous_end());
        self.tree.add_stmt(Stmt::For {
            name,
            name_span,
            start: range_start,
            end: range_end,
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

    /// A statement-level construct outside the v0 subset: diagnose and skip a
    /// following balanced block if present, leaving a `Stmt::Error`.
    fn parse_unsupported_stmt(&mut self) -> StmtId {
        let start = self.current().span;
        let keyword = self.current_kind().describe();
        self.error(
            start,
            "KSEM901",
            format!("{keyword} statements are not supported yet"),
        );
        self.bump();
        // Skip up to and including a `{...}` body when the construct has one.
        while !self.at_eof()
            && !self.at(TokenKind::RBrace)
            && !self.at(TokenKind::Semicolon)
            && !self.at(TokenKind::LBrace)
        {
            self.bump();
        }
        if self.at(TokenKind::LBrace) {
            self.skip_balanced(TokenKind::LBrace, TokenKind::RBrace);
        }
        let span = Span::from_bounds(start.start, self.previous_end());
        self.tree.add_stmt(Stmt::Error { span })
    }

    /// Whether the current token can begin an expression (used to decide
    /// whether `return` carries a value).
    fn starts_expression(&self) -> bool {
        matches!(
            self.current_kind(),
            TokenKind::IntLiteral
                | TokenKind::FloatLiteral
                | TokenKind::StringLiteral
                | TokenKind::True
                | TokenKind::False
                | TokenKind::Identifier
                | TokenKind::LParen
                | TokenKind::Minus
                | TokenKind::Bang
        )
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
        let function = match &result.tree.items[0] {
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

    #[test]
    fn a_for_loop_parses_its_variable_and_both_bounds() {
        let (_, stmts) = body("function f() { for i in 0..5 { } }");
        match &stmts[0] {
            Stmt::For { body, .. } => assert!(body.stmts.is_empty()),
            other => panic!("expected a `for`, got {other:?}"),
        }
    }

    /// The body brace must read as a block, not as a struct literal on the
    /// upper bound — the same ambiguity a `while` condition has.
    #[test]
    fn a_for_bound_does_not_swallow_the_body_brace() {
        let (tree, stmts) = body("function f() { for i in 0..n { let x = 1 } }");
        match &stmts[0] {
            Stmt::For { end, body, .. } => {
                assert_eq!(body.stmts.len(), 1, "the brace is the loop body");
                assert!(
                    matches!(tree.expr(*end), kira_syntax_model::ast::Expr::Name { .. }),
                    "the upper bound is the bare name, not a literal"
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

    /// A `for` missing its `..` is reported rather than silently reading the
    /// lower bound as the whole range.
    #[test]
    fn a_for_without_a_range_is_reported() {
        let result = parse(SourceId::new(0), "function f() { for i in 0 { } }");
        assert!(!result.diagnostics.is_empty());
    }

    /// Recovery: a broken `for` header still leaves a parseable program.
    #[test]
    fn a_for_without_a_loop_variable_still_parses_the_program() {
        let result = parse(SourceId::new(0), "function f() { for in 0..5 { } }");
        assert!(!result.diagnostics.is_empty());
        assert_eq!(result.tree.items.len(), 1, "the function is still there");
    }
}
