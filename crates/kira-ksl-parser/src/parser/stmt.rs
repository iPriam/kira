//! Blocks and statements.
//!
//! KSL has no statement terminator: a newline ends a statement and the grammar
//! is unambiguous without one, because every statement starts with a keyword
//! or a place expression. So nothing here looks for a separator — a statement
//! ends where the next one can begin.

use kira_ksl_syntax_model::ast::{Block, Stmt};
use kira_ksl_syntax_model::token::TokenKind;
use kira_ksl_syntax_model::tree::StmtId;

use super::Parser;

impl Parser<'_> {
    /// `{ statements }`
    pub(crate) fn block(&mut self) -> Option<Block> {
        let start = self.expect(TokenKind::LBrace)?;
        let mut stmts = Vec::new();
        while !self.at_end() && self.current() != TokenKind::RBrace {
            let before = self.at_index();
            match self.stmt() {
                Some(id) => stmts.push(id),
                None => {
                    if self.at_index() == before {
                        self.advance();
                    }
                }
            }
        }
        self.expect(TokenKind::RBrace);
        Some(Block {
            stmts,
            span: self.since(start),
        })
    }

    /// One statement.
    fn stmt(&mut self) -> Option<StmtId> {
        match self.current() {
            TokenKind::Let => self.let_stmt(),
            TokenKind::If => self.if_stmt(),
            TokenKind::While => self.while_stmt(),
            TokenKind::Return => self.return_stmt(),
            TokenKind::LBrace => {
                let block = self.block()?;
                Some(self.tree.stmts.alloc(Stmt::Block(block)))
            }
            _ => self.expr_or_assign(),
        }
    }

    /// `let name: Type = value`, with either half omitted.
    fn let_stmt(&mut self) -> Option<StmtId> {
        let start = self.advance();
        let (name, _) = self.expect_name()?;
        let ty = if self.eat(TokenKind::Colon) {
            Some(self.type_ref()?)
        } else {
            None
        };
        let init = if self.eat(TokenKind::Equals) {
            Some(self.expr()?)
        } else {
            None
        };
        let span = self.since(start);
        Some(self.tree.stmts.alloc(Stmt::Let {
            name,
            ty,
            init,
            span,
        }))
    }

    /// `if cond { … }`, with `else` and `else if` chains.
    fn if_stmt(&mut self) -> Option<StmtId> {
        let start = self.advance();
        let cond = self.expr()?;
        let then = self.block()?;
        let otherwise = if self.eat(TokenKind::Else) {
            if self.current() == TokenKind::If {
                self.if_stmt()
            } else {
                let block = self.block()?;
                Some(self.tree.stmts.alloc(Stmt::Block(block)))
            }
        } else {
            None
        };
        let span = self.since(start);
        Some(self.tree.stmts.alloc(Stmt::If {
            cond,
            then,
            otherwise,
            span,
        }))
    }

    /// `while cond { … }`
    fn while_stmt(&mut self) -> Option<StmtId> {
        let start = self.advance();
        let cond = self.expr()?;
        let body = self.block()?;
        let span = self.since(start);
        Some(self.tree.stmts.alloc(Stmt::While { cond, body, span }))
    }

    /// `return`, with or without a value.
    ///
    /// A bare `return` is told from `return value` by what follows it: a value
    /// can only start where an expression can, and a `}` cannot.
    fn return_stmt(&mut self) -> Option<StmtId> {
        let start = self.advance();
        let value = if starts_expression(self.current()) {
            Some(self.expr()?)
        } else {
            None
        };
        let span = self.since(start);
        Some(self.tree.stmts.alloc(Stmt::Return { value, span }))
    }

    /// An expression statement, or an assignment when a `=` follows the place.
    fn expr_or_assign(&mut self) -> Option<StmtId> {
        let start = self.span();
        let first = self.expr()?;
        if self.eat(TokenKind::Equals) {
            let value = self.expr()?;
            let span = self.since(start);
            return Some(self.tree.stmts.alloc(Stmt::Assign {
                target: first,
                value,
                span,
            }));
        }
        let span = self.since(start);
        Some(self.tree.stmts.alloc(Stmt::Expr { expr: first, span }))
    }
}

/// Whether `kind` can open an expression.
fn starts_expression(kind: TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Identifier
            | TokenKind::IntLiteral
            | TokenKind::FloatLiteral
            | TokenKind::True
            | TokenKind::False
            | TokenKind::LParen
            | TokenKind::Minus
            | TokenKind::Bang
    )
}
