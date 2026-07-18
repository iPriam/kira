//! Expression parsing via precedence climbing.
//!
//! Binding powers encode Kira's operator precedence (lowest to highest):
//! `||`, `&&`, `|`, `^`, `&`, `==`/`!=`, the four orderings, `<<`/`>>`,
//! `+`/`-`, `*`/`/`/`%`, then prefix `-`/`!`/`~`, then primaries and call
//! postfixes. All binary operators are left-associative.
//!
//! That ladder is C's, rung for rung. Two levels are still worth stating,
//! because Go and Swift moved them and getting them wrong changes what an
//! accepted program means: the bitwise operators bind **looser** than equality
//! (so `a & b == c` is `a & (b == c)`, not `(a & b) == c` — C's classic wart),
//! and the shifts bind **tighter** than the orderings but looser than `+`/`-`
//! (so `a + b << c` is `(a + b) << c`).
//!
//! The conditional `? :` sits below every binary operator and is the one
//! right-associative form: `a ? b : c ? d : e` groups as `a ? b : (c ? d : e)`,
//! because each branch is parsed as a full expression.

use kira_lexer::decode_string_literal;
use kira_source::Span;
use kira_syntax_model::TokenKind;
use kira_syntax_model::ast::{BinaryOp, Expr, ExprId, UnaryOp};
use kira_syntax_model::ownership::OwnershipOp;

use crate::Parser;

mod aggregates;
mod closures;

impl Parser<'_> {
    /// Parses a full expression.
    pub(crate) fn parse_expr(&mut self) -> ExprId {
        let cond = self.parse_binary(0);
        if !self.at(TokenKind::Question) {
            return cond;
        }
        self.bump(); // `?`
        // Both branches are full expressions, which is what makes the form
        // right-associative and lets a nested `? :` sit in the else position
        // without parentheses.
        let then = self.parse_expr();
        self.expect(TokenKind::Colon);
        let otherwise = self.parse_expr();
        let span = Span::from_bounds(
            self.tree.expr(cond).span().start,
            self.tree.expr(otherwise).span().end(),
        );
        self.tree.add_expr(Expr::Conditional {
            cond,
            then,
            otherwise,
            span,
        })
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

    /// Whether the cursor sits on a `move`/`copy` that is acting as an
    /// ownership operator rather than as a plain name.
    ///
    /// The whole contextual-identifier discipline is this lookahead: the token
    /// is an operator only when what follows it starts an operand. `move x` is
    /// a transfer; `move` alone, `move + 1`, and `move.field` are reads of a
    /// local named `move`.
    fn ownership_op_here(&self) -> Option<OwnershipOp> {
        let op = if self.at_word("move") {
            OwnershipOp::Move
        } else if self.at_word("copy") {
            OwnershipOp::Copy
        } else {
            return None;
        };
        let starts_operand = matches!(
            self.peek(1).kind,
            TokenKind::Identifier
                | TokenKind::IntLiteral
                | TokenKind::FloatLiteral
                | TokenKind::StringLiteral
                | TokenKind::True
                | TokenKind::False
                | TokenKind::Minus
                | TokenKind::Bang
                | TokenKind::Tilde
                | TokenKind::LBracket
        );
        starts_operand.then_some(op)
    }

    fn parse_unary(&mut self) -> ExprId {
        if let Some(op) = self.ownership_op_here() {
            let start = self.current().span;
            self.bump(); // `move` / `copy`
            let operand = self.parse_unary();
            let operand_span = self.tree.expr(operand).span();
            let span = Span::from_bounds(start.start, operand_span.end());
            return self.tree.add_expr(Expr::Ownership { op, operand, span });
        }
        let op = match self.current_kind() {
            TokenKind::Minus => Some(UnaryOp::Neg),
            TokenKind::Bang => Some(UnaryOp::Not),
            TokenKind::Tilde => Some(UnaryOp::BitNot),
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

    /// Applies postfix `.field` accesses and `[index]` reads to an
    /// already-parsed expression.
    ///
    /// Chains left-associatively, so `b.size.x` reads the `size` field of `b`
    /// and then the `x` field of that, and `grid[0].cells[2]` walks the same
    /// way — which is what lets one loop handle a path of any shape rather than
    /// fields and indices needing their own grammars.
    fn parse_postfix(&mut self, mut base: ExprId) -> ExprId {
        loop {
            if self.at(TokenKind::Dot) {
                match self.parse_dot_postfix(base) {
                    Ok(next) => base = next,
                    Err(error) => return error,
                }
                continue;
            }
            if self.at(TokenKind::LBracket) {
                let base_span = self.tree.expr(base).span();
                self.bump(); // `[`
                // An index is a value, not a condition: a struct literal is
                // legal here even when this sits in one of the positions where
                // a bare `{` would end the expression.
                let index = self.with_struct_literals(|parser| parser.parse_expr());
                self.expect(TokenKind::RBracket);
                let span = Span::from_bounds(base_span.start, self.previous_end());
                base = self.tree.add_expr(Expr::Index { base, index, span });
                continue;
            }
            // A trailing closure is the call's last argument. It is gated on
            // struct literals being permitted because both answer the same
            // question — whether a `{` after an expression belongs to the
            // expression or opens the body of an enclosing `if`/`while`/`for`.
            if !self.no_struct_literal && self.at_closure_start() {
                base = self.attach_trailing_closure(base);
                continue;
            }
            break;
        }
        base
    }

    /// Parses one `.field` or `.method(...)` step, with the cursor on `.`.
    ///
    /// `Err` carries the error node a malformed step recovers to, which ends
    /// the postfix chain rather than continuing it.
    fn parse_dot_postfix(&mut self, base: ExprId) -> Result<ExprId, ExprId> {
        self.bump(); // `.`
        if !self.at(TokenKind::Identifier) {
            let span = self.current().span;
            self.error(span, "KPAR022", "expected a field name after `.`");
            return Err(self.error_expr(span));
        }
        let field_span = self.current().span;
        let field = self.intern_span(field_span);
        self.bump();
        let base_span = self.tree.expr(base).span();
        // `p.sum()` is a method call; `p.x` is a field read. The `(` is the
        // whole difference, so it is what decides. `xs.count` is a *property*
        // and takes this same field-read path — analysis is what knows an
        // array has no fields but does have a count.
        if self.at(TokenKind::LParen) {
            let args = self.parse_call_args();
            let span = Span::from_bounds(base_span.start, self.previous_end());
            return Ok(self.tree.add_expr(Expr::MethodCall {
                receiver: base,
                method: field,
                method_span: field_span,
                args,
                span,
            }));
        }
        let span = Span::from_bounds(base_span.start, field_span.end());
        Ok(self.tree.add_expr(Expr::Field {
            base,
            field,
            field_span,
            span,
        }))
    }

    fn parse_primary(&mut self) -> ExprId {
        let token = self.current();
        // A closure literal stands on its own wherever a value does — a `let`
        // initializer, a `return`, an argument. Unlike the trailing form this
        // is *not* gated on struct literals: `{ x in … }` cannot be confused
        // with a control-flow body, because a body never follows nothing.
        if self.at_closure_start() {
            return self.parse_closure();
        }
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
            // A leading dot is a member of the expected type (`.Green`,
            // `.Ok(12)`) — an enum variant, in the v0 subset. It only starts a
            // primary when a name follows; a bare `.` is a malformed field
            // access, reported as one.
            TokenKind::Dot if self.peek(1).kind == TokenKind::Identifier => {
                self.parse_dot_member(token.span)
            }
            TokenKind::LParen => self.parse_paren(),
            TokenKind::LBracket => self.parse_array_literal(),
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

    /// Parses a leading-dot member (`.Green`, `.Ok(12)`), cursor on the `.`.
    ///
    /// The `(` after the name is the whole difference between a payload-less
    /// variant and one carrying a payload, so it is what decides which is
    /// parsed. What the member resolves against is the expected type, which only
    /// analysis knows — the parser records the name and its arguments and stops
    /// there.
    fn parse_dot_member(&mut self, dot_span: Span) -> ExprId {
        self.bump(); // `.`
        let name_span = self.current().span;
        let name = self.intern_span(name_span);
        self.bump(); // member name
        let args = if self.at(TokenKind::LParen) {
            Some(self.parse_call_args())
        } else {
            None
        };
        let span = Span::from_bounds(dot_span.start, self.previous_end());
        self.tree.add_expr(Expr::DotMember {
            name,
            name_span,
            args,
            span,
        })
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
        } else if self.at(TokenKind::LBrace) && !self.no_struct_literal && !self.at_closure_start()
        {
            self.parse_struct_literal(symbol, name_span)
        } else {
            self.tree.add_expr(Expr::Name {
                symbol,
                span: name_span,
            })
        }
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
        TokenKind::Pipe => (BinaryOp::BitOr, 3),
        TokenKind::Caret => (BinaryOp::BitXor, 4),
        TokenKind::Amp => (BinaryOp::BitAnd, 5),
        TokenKind::EqEq => (BinaryOp::Eq, 6),
        TokenKind::BangEq => (BinaryOp::Ne, 6),
        TokenKind::Lt => (BinaryOp::Lt, 7),
        TokenKind::LtEq => (BinaryOp::Le, 7),
        TokenKind::Gt => (BinaryOp::Gt, 7),
        TokenKind::GtEq => (BinaryOp::Ge, 7),
        TokenKind::LtLt => (BinaryOp::Shl, 8),
        TokenKind::GtGt => (BinaryOp::Shr, 8),
        TokenKind::Plus => (BinaryOp::Add, 9),
        TokenKind::Minus => (BinaryOp::Sub, 9),
        TokenKind::Star => (BinaryOp::Mul, 10),
        TokenKind::Slash => (BinaryOp::Div, 10),
        TokenKind::Percent => (BinaryOp::Rem, 10),
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
        let function = match &tree.items()[0] {
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
            Expr::Conditional {
                cond,
                then,
                otherwise,
                ..
            } => format!(
                "({} ? {} : {})",
                render(tree, *cond, interner),
                render(tree, *then, interner),
                render(tree, *otherwise, interner)
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

    // The bitwise ladder. Each of these groups as C groups it, and as Go and
    // Swift do not, so they pin the rungs a contributor is most likely to
    // "correct" from memory.

    #[test]
    fn bitwise_and_binds_tighter_than_xor_and_or() {
        assert_eq!(
            return_shape("function f() { return 1 | 2 ^ 3 & 4 }"),
            "(1 | (2 ^ (3 & 4)))"
        );
    }

    #[test]
    fn bitwise_or_binds_looser_than_equality() {
        // C reads this the same way; Go and Swift read it as `(1 | 2) == 3`.
        assert_eq!(
            return_shape("function f() { return 1 | 2 == 3 }"),
            "(1 | (2 == 3))"
        );
    }

    #[test]
    fn bitwise_binds_tighter_than_logical_and() {
        assert_eq!(
            return_shape("function f() { return true && 1 | 2 }"),
            "(true && (1 | 2))"
        );
    }

    #[test]
    fn shift_binds_tighter_than_comparison_and_looser_than_addition() {
        assert_eq!(
            return_shape("function f() { return 1 + 2 << 3 < 4 }"),
            "(((1 + 2) << 3) < 4)"
        );
    }

    #[test]
    fn shift_is_left_associative() {
        assert_eq!(
            return_shape("function f() { return 1 << 2 >> 3 }"),
            "((1 << 2) >> 3)"
        );
    }

    #[test]
    fn complement_binds_tighter_than_bitwise_and() {
        assert_eq!(
            return_shape("function f() { return ~1 & 2 }"),
            "((BitNot 1) & 2)"
        );
    }

    // The conditional.

    #[test]
    fn conditional_binds_looser_than_every_binary_operator() {
        assert_eq!(
            return_shape("function f() { return a || b ? 1 + 2 : 3 | 4 }"),
            "((a || b) ? (1 + 2) : (3 | 4))"
        );
    }

    #[test]
    fn conditional_is_right_associative() {
        assert_eq!(
            return_shape("function f() { return a ? 1 : b ? 2 : 3 }"),
            "(a ? 1 : (b ? 2 : 3))"
        );
    }

    #[test]
    fn conditional_nests_in_its_then_branch() {
        assert_eq!(
            return_shape("function f() { return a ? b ? 1 : 2 : 3 }"),
            "(a ? (b ? 1 : 2) : 3)"
        );
    }

    /// A conditional missing its `:` recovers rather than derailing the parse:
    /// the error is reported and the function still yields one return
    /// statement, so later items keep being parsed.
    #[test]
    fn conditional_without_colon_recovers() {
        let result = parse(SourceId::new(0), "function f() { return a ? 1 2 }");
        assert!(
            !result.diagnostics.is_empty(),
            "a missing `:` must be diagnosed"
        );
        let function = match &result.tree.items()[0] {
            Item::Function(f) => f,
            other => panic!("expected function, got {other:?}"),
        };
        assert!(
            !function.body.stmts.is_empty(),
            "the function body must survive the error"
        );
    }
}
