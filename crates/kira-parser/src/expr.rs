//! Expression parsing via precedence climbing.
//!
//! Binding powers encode Kira's operator precedence (lowest to highest): `||`,
//! `&&`, comparisons, `+`/`-`, `*`/`/`/`%`, then prefix `-`/`!`, then primaries
//! and call postfixes. All binary operators are left-associative.

use kira_lexer::decode_string_literal;
use kira_source::Span;
use kira_syntax_model::TokenKind;
use kira_syntax_model::ast::{BinaryOp, Expr, ExprId, FieldInit, UnaryOp};

use crate::Parser;

impl Parser<'_> {
    /// Parses a full expression.
    pub(crate) fn parse_expr(&mut self) -> ExprId {
        self.parse_binary(0)
    }

    /// Builds a placeholder error expression node.
    pub(crate) fn error_expr(&mut self, span: Span) -> ExprId {
        self.tree.add_expr(Expr::Error { span })
    }

    fn parse_binary(&mut self, min_bp: u8) -> ExprId {
        let mut lhs = self.parse_unary();
        while let Some((op, bp)) = binary_op(self.current_kind()) {
            if bp <= min_bp {
                break;
            }
            self.bump(); // operator
            let rhs = self.parse_binary(bp);
            let lhs_span = self.tree.expr(lhs).span();
            let rhs_span = self.tree.expr(rhs).span();
            let span = Span::from_bounds(lhs_span.start, rhs_span.end());
            lhs = self.tree.add_expr(Expr::Binary { op, lhs, rhs, span });
        }
        lhs
    }

    fn parse_unary(&mut self) -> ExprId {
        let op = match self.current_kind() {
            TokenKind::Minus => Some(UnaryOp::Neg),
            TokenKind::Bang => Some(UnaryOp::Not),
            _ => None,
        };
        if let Some(op) = op {
            let start = self.current().span;
            self.bump();
            let operand = self.parse_unary();
            let operand_span = self.tree.expr(operand).span();
            let span = Span::from_bounds(start.start, operand_span.end());
            return self.tree.add_expr(Expr::Unary { op, operand, span });
        }
        let primary = self.parse_primary();
        self.parse_postfix(primary)
    }

    /// Applies postfix `.field` accesses to an already-parsed expression.
    ///
    /// Chains left-associatively, so `b.size.x` reads the `size` field of `b`
    /// and then the `x` field of that.
    fn parse_postfix(&mut self, mut base: ExprId) -> ExprId {
        while self.at(TokenKind::Dot) {
            self.bump(); // `.`
            if !self.at(TokenKind::Identifier) {
                let span = self.current().span;
                self.error(span, "KPAR022", "expected a field name after `.`");
                return self.error_expr(span);
            }
            let field_span = self.current().span;
            let field = self.intern_span(field_span);
            self.bump();
            let base_span = self.tree.expr(base).span();
            // `p.sum()` is a method call; `p.x` is a field read. The `(` is the
            // whole difference, so it is what decides.
            if self.at(TokenKind::LParen) {
                let args = self.parse_call_args();
                let span = Span::from_bounds(base_span.start, self.previous_end());
                base = self.tree.add_expr(Expr::MethodCall {
                    receiver: base,
                    method: field,
                    method_span: field_span,
                    args,
                    span,
                });
                continue;
            }
            let span = Span::from_bounds(base_span.start, field_span.end());
            base = self.tree.add_expr(Expr::Field {
                base,
                field,
                field_span,
                span,
            });
        }
        base
    }

    fn parse_primary(&mut self) -> ExprId {
        let token = self.current();
        match token.kind {
            TokenKind::IntLiteral => self.parse_int(token.span),
            TokenKind::FloatLiteral => self.parse_float(token.span),
            TokenKind::StringLiteral => {
                self.bump();
                let value = decode_string_literal(self.text_of(token.span));
                self.tree.add_expr(Expr::Str {
                    value,
                    span: token.span,
                })
            }
            TokenKind::True => {
                self.bump();
                self.tree.add_expr(Expr::Bool {
                    value: true,
                    span: token.span,
                })
            }
            TokenKind::False => {
                self.bump();
                self.tree.add_expr(Expr::Bool {
                    value: false,
                    span: token.span,
                })
            }
            TokenKind::Identifier => self.parse_name_or_call(token.span),
            TokenKind::LParen => self.parse_paren(),
            _ => {
                self.error(
                    token.span,
                    "KPAR020",
                    format!("expected an expression, found {}", token.kind.describe()),
                );
                self.bump();
                self.error_expr(token.span)
            }
        }
    }

    fn parse_int(&mut self, span: Span) -> ExprId {
        self.bump();
        let text = self.text_of(span);
        let value = match text.parse::<i64>() {
            Ok(value) => value,
            Err(_) => {
                self.error(
                    span,
                    "KPAR021",
                    "integer literal does not fit in a 64-bit integer",
                );
                0
            }
        };
        self.tree.add_expr(Expr::Int { value, span })
    }

    fn parse_float(&mut self, span: Span) -> ExprId {
        self.bump();
        let text = self.text_of(span);
        let value = text.parse::<f64>().unwrap_or(0.0);
        self.tree.add_expr(Expr::Float { value, span })
    }

    fn parse_name_or_call(&mut self, name_span: Span) -> ExprId {
        let symbol = self.intern_span(name_span);
        self.bump();
        if self.at(TokenKind::LParen) {
            let args = self.parse_call_args();
            let span = Span::from_bounds(name_span.start, self.previous_end());
            self.tree.add_expr(Expr::Call {
                callee: symbol,
                callee_span: name_span,
                args,
                span,
            })
        } else if self.at(TokenKind::LBrace) && !self.no_struct_literal {
            self.parse_struct_literal(symbol, name_span)
        } else {
            self.tree.add_expr(Expr::Name {
                symbol,
                span: name_span,
            })
        }
    }

    /// Parses the `{ … }` of a struct literal, with the cursor on `{`.
    ///
    /// Field initializers are separated by `,` or by nothing at all — newlines
    /// are insignificant, so a separator cannot be required. Both binders are
    /// accepted: `=` is canonical, `:` stays valid for the transition window,
    /// and the two may be mixed in one literal.
    fn parse_struct_literal(&mut self, name: kira_core::Symbol, name_span: Span) -> ExprId {
        self.bump(); // `{`
        let mut fields = Vec::new();
        // A literal's fields are values, not conditions: a nested literal is
        // legal here even when this one sits in a condition.
        self.with_struct_literals(|parser| {
            while !parser.at(TokenKind::RBrace) && !parser.at_eof() {
                let before = parser.pos;
                while parser.eat(TokenKind::Semicolon) {}
                if parser.at(TokenKind::RBrace) || parser.at_eof() {
                    break;
                }
                if let Some(field) = parser.parse_field_init() {
                    fields.push(field);
                }
                parser.eat(TokenKind::Comma);
                if parser.pos == before {
                    parser.bump();
                }
            }
        });
        self.expect(TokenKind::RBrace);
        let span = Span::from_bounds(name_span.start, self.previous_end());
        self.tree.add_expr(Expr::StructLit {
            name,
            name_span,
            fields,
            span,
        })
    }

    /// Parses one `name = value` / `name: value` field initializer.
    fn parse_field_init(&mut self) -> Option<FieldInit> {
        if !self.at(TokenKind::Identifier) {
            let span = self.current().span;
            self.error(
                span,
                "KPAR023",
                format!(
                    "expected a field name in a struct literal, found {}",
                    self.current_kind().describe()
                ),
            );
            self.bump();
            return None;
        }
        let name_span = self.current().span;
        let name = self.intern_span(name_span);
        self.bump();
        if !self.eat(TokenKind::Equals) && !self.eat(TokenKind::Colon) {
            let span = self.current().span;
            self.error(
                span,
                "KPAR024",
                "expected `=` after a field name in a struct literal",
            );
            return None;
        }
        let value = self.parse_expr();
        let span = Span::from_bounds(name_span.start, self.previous_end());
        Some(FieldInit {
            name,
            name_span,
            value,
            span,
        })
    }

    fn parse_call_args(&mut self) -> Vec<ExprId> {
        let mut args = Vec::new();
        self.bump(); // `(`
        self.with_struct_literals(|parser| {
            while !parser.at(TokenKind::RParen) && !parser.at_eof() {
                let before = parser.pos;
                args.push(parser.parse_expr());
                if !parser.eat(TokenKind::Comma) {
                    break;
                }
                if parser.pos == before {
                    parser.bump();
                }
            }
        });
        self.expect(TokenKind::RParen);
        args
    }

    fn parse_paren(&mut self) -> ExprId {
        self.bump(); // `(`
        let inner = self.with_struct_literals(|parser| parser.parse_expr());
        self.expect(TokenKind::RParen);
        inner
    }
}

/// The operator and left binding power for a token that opens a binary
/// expression, or `None` when it is not a binary operator.
fn binary_op(kind: TokenKind) -> Option<(BinaryOp, u8)> {
    let pair = match kind {
        TokenKind::PipePipe => (BinaryOp::Or, 1),
        TokenKind::AmpAmp => (BinaryOp::And, 2),
        TokenKind::EqEq => (BinaryOp::Eq, 3),
        TokenKind::BangEq => (BinaryOp::Ne, 3),
        TokenKind::Lt => (BinaryOp::Lt, 3),
        TokenKind::LtEq => (BinaryOp::Le, 3),
        TokenKind::Gt => (BinaryOp::Gt, 3),
        TokenKind::GtEq => (BinaryOp::Ge, 3),
        TokenKind::Plus => (BinaryOp::Add, 4),
        TokenKind::Minus => (BinaryOp::Sub, 4),
        TokenKind::Star => (BinaryOp::Mul, 5),
        TokenKind::Slash => (BinaryOp::Div, 5),
        TokenKind::Percent => (BinaryOp::Rem, 5),
        _ => return None,
    };
    Some(pair)
}

#[cfg(test)]
mod tests {
    use crate::parse;
    use kira_source::SourceId;
    use kira_syntax_model::SyntaxTree;
    use kira_syntax_model::ast::{BinaryOp, Expr, Item, Stmt};

    /// Renders the first return-statement expression of the first function as
    /// a fully-parenthesized string, to assert precedence shape.
    fn return_shape(text: &str) -> String {
        let result = parse(SourceId::new(0), text);
        let tree = &result.tree;
        let function = match &tree.items[0] {
            Item::Function(f) => f,
            other => panic!("expected function, got {other:?}"),
        };
        let stmt_id = *function.body.stmts.first().expect("a statement");
        let expr = match tree.stmt(stmt_id) {
            Stmt::Return {
                value: Some(expr), ..
            } => *expr,
            other => panic!("expected return with value, got {other:?}"),
        };
        render(tree, expr, &result.interner)
    }

    fn render(
        tree: &SyntaxTree,
        id: kira_syntax_model::ast::ExprId,
        interner: &kira_core::Interner,
    ) -> String {
        match tree.expr(id) {
            Expr::Int { value, .. } => value.to_string(),
            Expr::Bool { value, .. } => value.to_string(),
            Expr::Name { symbol, .. } => interner.resolve(*symbol).to_owned(),
            Expr::Unary { op, operand, .. } => {
                format!("({:?} {})", op, render(tree, *operand, interner))
            }
            Expr::Binary { op, lhs, rhs, .. } => format!(
                "({} {} {})",
                render(tree, *lhs, interner),
                spelling(*op),
                render(tree, *rhs, interner)
            ),
            other => format!("{other:?}"),
        }
    }

    fn spelling(op: BinaryOp) -> &'static str {
        op.spelling()
    }

    #[test]
    fn multiplication_binds_tighter_than_addition() {
        assert_eq!(
            return_shape("function f() { return 2 + 3 * 4 }"),
            "(2 + (3 * 4))"
        );
    }

    #[test]
    fn subtraction_is_left_associative() {
        assert_eq!(
            return_shape("function f() { return 10 - 2 - 3 }"),
            "((10 - 2) - 3)"
        );
    }

    #[test]
    fn comparison_below_arithmetic_and_logic_below_comparison() {
        assert_eq!(
            return_shape("function f() { return 1 + 2 > 2 && 3 < 5 }"),
            "(((1 + 2) > 2) && (3 < 5))"
        );
    }

    #[test]
    fn and_binds_tighter_than_or() {
        assert_eq!(
            return_shape("function f() { return true || false && false }"),
            "(true || (false && false))"
        );
    }

    #[test]
    fn unary_binds_tighter_than_multiplication() {
        assert_eq!(
            return_shape("function f() { return 2 * -3 }"),
            "(2 * (Neg 3))"
        );
    }
}
