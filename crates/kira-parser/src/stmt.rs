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
            TokenKind::For | TokenKind::Switch | TokenKind::Match => {
                Some(self.parse_unsupported_stmt())
            }
            TokenKind::Break | TokenKind::Continue => Some(self.parse_unsupported_stmt()),
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
