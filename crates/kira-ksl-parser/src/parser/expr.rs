//! Expressions, by precedence climbing.
//!
//! One ladder, taken from [`BinaryOp::precedence`], so adding an operator is a
//! change to the model rather than to the parser.

use kira_ksl_syntax_model::ast::{BinaryOp, Expr, UnaryOp};
use kira_ksl_syntax_model::token::TokenKind;
use kira_ksl_syntax_model::tree::ExprId;

use super::Parser;
use crate::diagnostics;

impl Parser<'_> {
    /// One whole expression.
    pub(crate) fn expr(&mut self) -> Option<ExprId> {
        self.binary(0)
    }

    /// Every operator binding at least as tightly as `floor`.
    fn binary(&mut self, floor: u8) -> Option<ExprId> {
        let mut left = self.unary()?;
        while let Some(op) = binary_op(self.current()) {
            let precedence = op.precedence();
            if precedence < floor {
                break;
            }
            self.advance();
            // Left-associative: the right operand may only take operators that
            // bind strictly tighter, so `a - b - c` groups as `(a - b) - c`.
            let right = self.binary(precedence + 1)?;
            let span = self.spanning(left, right);
            left = self.tree.exprs.alloc(Expr::Binary {
                op,
                lhs: left,
                rhs: right,
                span,
            });
        }
        Some(left)
    }

    /// A prefix operator, or a postfix chain.
    fn unary(&mut self) -> Option<ExprId> {
        let op = match self.current() {
            TokenKind::Minus => UnaryOp::Neg,
            TokenKind::Bang => UnaryOp::Not,
            _ => return self.postfix(),
        };
        let start = self.advance();
        let operand = self.unary()?;
        let span = self.since(start);
        Some(self.tree.exprs.alloc(Expr::Unary { op, operand, span }))
    }

    /// A primary followed by any number of `.field`, `[index]`, and `(args)`.
    fn postfix(&mut self) -> Option<ExprId> {
        let start = self.span();
        let mut base = self.primary()?;
        loop {
            match self.current() {
                TokenKind::Dot => {
                    self.advance();
                    let (field, _) = self.expect_name()?;
                    let span = self.since(start);
                    base = self.tree.exprs.alloc(Expr::Field { base, field, span });
                }
                TokenKind::LBracket => {
                    self.advance();
                    let index = self.expr()?;
                    self.expect(TokenKind::RBracket)?;
                    let span = self.since(start);
                    base = self.tree.exprs.alloc(Expr::Index { base, index, span });
                }
                TokenKind::LParen => {
                    self.advance();
                    let mut args = Vec::new();
                    while !self.at_end() && self.current() != TokenKind::RParen {
                        args.push(self.expr()?);
                        if !self.eat(TokenKind::Comma) {
                            break;
                        }
                    }
                    self.expect(TokenKind::RParen)?;
                    let span = self.since(start);
                    base = self.tree.exprs.alloc(Expr::Call {
                        callee: base,
                        args,
                        span,
                    });
                }
                _ => break,
            }
        }
        Some(base)
    }

    /// A literal, a name, or a parenthesized expression.
    fn primary(&mut self) -> Option<ExprId> {
        let span = self.span();
        match self.current() {
            TokenKind::IntLiteral => {
                let text = self.slice().to_owned();
                self.advance();
                let value = match text.parse::<u64>() {
                    Ok(value) => value,
                    Err(_) => {
                        self.reporter.error(
                            span,
                            diagnostics::LITERAL_RANGE,
                            format!("`{text}` does not fit in a 64-bit integer"),
                        );
                        0
                    }
                };
                Some(self.tree.exprs.alloc(Expr::Int { value, span }))
            }
            TokenKind::FloatLiteral => {
                let text = self.slice().to_owned();
                self.advance();
                let value = match text.parse::<f64>() {
                    Ok(value) => value,
                    Err(_) => {
                        self.reporter.error(
                            span,
                            diagnostics::LITERAL_RANGE,
                            format!("`{text}` is not a number this compiler can represent"),
                        );
                        0.0
                    }
                };
                Some(self.tree.exprs.alloc(Expr::Float { value, span }))
            }
            TokenKind::True | TokenKind::False => {
                let value = self.current() == TokenKind::True;
                self.advance();
                Some(self.tree.exprs.alloc(Expr::Bool { value, span }))
            }
            TokenKind::Identifier => {
                let symbol = self.intern_current();
                Some(self.tree.exprs.alloc(Expr::Name { symbol, span }))
            }
            TokenKind::LParen => {
                self.advance();
                let inner = self.expr()?;
                self.expect(TokenKind::RParen)?;
                Some(inner)
            }
            other => {
                self.reporter.error(
                    span,
                    diagnostics::UNEXPECTED,
                    format!("expected a value, found {}", other.spelling()),
                );
                None
            }
        }
    }

    /// The span covering both operands of a binary expression.
    fn spanning(&self, left: ExprId, right: ExprId) -> kira_source::Span {
        let left = self.tree.expr(left).span();
        let right = self.tree.expr(right).span();
        kira_source::Span::new(left.start, right.end().saturating_sub(left.start))
    }
}

/// The binary operator `kind` spells, when it spells one.
fn binary_op(kind: TokenKind) -> Option<BinaryOp> {
    Some(match kind {
        TokenKind::Plus => BinaryOp::Add,
        TokenKind::Minus => BinaryOp::Sub,
        TokenKind::Star => BinaryOp::Mul,
        TokenKind::Slash => BinaryOp::Div,
        TokenKind::Percent => BinaryOp::Rem,
        TokenKind::EqualsEquals => BinaryOp::Eq,
        TokenKind::BangEquals => BinaryOp::Ne,
        TokenKind::Less => BinaryOp::Lt,
        TokenKind::LessEquals => BinaryOp::Le,
        TokenKind::Greater => BinaryOp::Gt,
        TokenKind::GreaterEquals => BinaryOp::Ge,
        TokenKind::AmpAmp => BinaryOp::And,
        TokenKind::PipePipe => BinaryOp::Or,
        TokenKind::Amp => BinaryOp::BitAnd,
        TokenKind::Pipe => BinaryOp::BitOr,
        TokenKind::Caret => BinaryOp::BitXor,
        TokenKind::LessLess => BinaryOp::Shl,
        TokenKind::GreaterGreater => BinaryOp::Shr,
        _ => return None,
    })
}
