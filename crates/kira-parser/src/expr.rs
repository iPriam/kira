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
use kira_syntax_model::ast::{BinaryOp, CallArg, Expr, ExprId, UnaryOp};
use kira_syntax_model::ownership::OwnershipOp;

use crate::Parser;

mod aggregates;
mod calls;
mod closures;
mod content;

impl Parser<'_> {
    /// Parses a full expression.
    ///
    /// Entry is guarded by the parser's nesting budget so pathological input
    /// cannot recurse to a stack overflow; see [`Parser::enter_nesting`].
    pub(crate) fn parse_expr(&mut self) -> ExprId {
        let allowed = self.enter_nesting();
        let expr = if allowed {
            self.parse_expr_descend()
        } else {
            self.recover_refused_nesting();
            self.error_expr(self.current().span)
        };
        self.exit_nesting();
        expr
    }

    fn parse_expr_descend(&mut self) -> ExprId {
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
        loop {
            // `value is Type` and `value as Type` bind tighter than a
            // comparison and looser than a shift, and their right side is a
            // type, not an expression.
            if matches!(self.current_kind(), TokenKind::Is | TokenKind::As)
                && TYPE_OPERATOR_BP > min_bp
            {
                let is_test = self.at(TokenKind::Is);
                self.bump(); // `is` / `as`
                let ty = self.parse_type_ref();
                let lhs_span = self.tree.expr(lhs).span();
                let span = Span::from_bounds(lhs_span.start, self.previous_end());
                lhs = self.tree.add_expr(if is_test {
                    Expr::TypeTest {
                        value: lhs,
                        ty,
                        span,
                    }
                } else {
                    Expr::TypeCast {
                        value: lhs,
                        ty,
                        span,
                    }
                });
                continue;
            }
            let Some((op, bp)) = binary_op(self.current_kind()) else {
                break;
            };
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

    /// Parses a prefix chain.
    ///
    /// One level deeper into a prefix chain, against the nesting budget.
    ///
    /// Every prefix operator recurses into [`Parser::parse_unary`] rather than
    /// into [`Parser::parse_expr`] — `try`, `move`/`copy`, `-`, `!` and `~` all
    /// descend directly — so a run of them never touched the budget and five
    /// thousand of them reached the stack's end instead of a diagnostic.
    ///
    /// The budget is spent on the RECURSION, not on entering `parse_unary`:
    /// every expression passes through it exactly once on its way to a primary,
    /// and charging that would halve the depth an ordinary parenthesized
    /// expression is allowed.
    /// What a `try` applies to: a unary expression and any `is`/`as` written on
    /// it, and nothing looser.
    ///
    /// Binding through the type operators is what makes `try value as T` the
    /// cast being tried. Stopping there is what keeps `try f() + 1` the sum of
    /// a tried call, rather than a try of a sum that has no failure to name.
    fn parse_try_operand(&mut self) -> ExprId {
        let allowed = self.enter_nesting();
        let expr = if allowed {
            self.parse_binary(TYPE_OPERATOR_BP - 1)
        } else {
            self.recover_refused_nesting();
            self.error_expr(self.current().span)
        };
        self.exit_nesting();
        expr
    }

    fn parse_unary_nested(&mut self) -> ExprId {
        let allowed = self.enter_nesting();
        let expr = if allowed {
            self.parse_unary()
        } else {
            self.recover_refused_nesting();
            self.error_expr(self.current().span)
        };
        self.exit_nesting();
        expr
    }

    fn parse_unary(&mut self) -> ExprId {
        // `try` binds like a prefix operator so `try f(n)` takes the whole
        // call, and looser than `is`/`as` so `try value as T` is the cast being
        // tried rather than a cast of what was tried. That is the only reading
        // the language has a meaning for: a `try` names the fallible step, and
        // the fallible step there is the cast. Whether this position is one
        // where a `try` is allowed at all is a question for analysis, not the
        // parser.
        if self.at(TokenKind::Try) {
            let start = self.current().span;
            self.bump(); // `try`
            let value = self.parse_try_operand();
            let value_span = self.tree.expr(value).span();
            let span = Span::from_bounds(start.start, value_span.end());
            return self.tree.add_expr(Expr::Try { value, span });
        }
        if let Some(op) = self.ownership_op_here() {
            let start = self.current().span;
            self.bump(); // `move` / `copy`
            let operand = self.parse_unary_nested();
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
            let operand = self.parse_unary_nested();
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
            // A trailing content block attaches children to a construction:
            // `HStack(spacing: 8) { Text("a") Text("b") }`. It follows a call
            // only, since a construction is a call — a name is promoted to a
            // call at the no-paren site, not here.
            if self.at_trailing_block()
                && let Some(next) = self.attach_content_block(base)
            {
                base = next;
                continue;
            }
            // Named child fills close a construction that ended with a block of
            // its own: `NavigationSplitView { … } detail: { … }`. Each becomes a
            // labeled argument, so analysis reads a fill the way it reads an
            // override written inside the block.
            if self.at_named_fill() && self.at_fillable_construction(base) {
                base = self.attach_named_fills(base);
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
        // `value.type` reads the runtime type descriptor, and `type` is a
        // keyword because a declaration starts with one. After a `.` there is
        // no declaration to start, so the keyword is a member name here and
        // nowhere else — the same reading every language with `.type` gives it.
        if !self.at(TokenKind::Identifier) && !self.at(TokenKind::Type) {
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
                children: Vec::new(),
                span,
            }));
        }
        // `Module.Type { … }` is a module-qualified struct literal. The parser
        // cannot resolve the module, so it keeps the qualifier in the interned
        // name — a dot cannot appear in an identifier, so the dotted spelling
        // can never collide with a declared one — exactly as a qualified *type*
        // reference does, and semantics strips it against the file's imports.
        // The guard mirrors a bare `Name { … }`: a `{` in a condition opens a
        // block, so a qualified literal there is written with parentheses too,
        // and a `{ x in … }` is a trailing closure, not a literal.
        if self.at(TokenKind::LBrace)
            && !self.no_struct_literal
            && !self.at_closure_start()
            && let Some(prefix) = self.name_path_text(base)
            && prefix != "MainThread"
        {
            let qualified = format!("{prefix}.{}", self.text_of(field_span));
            let name_span = Span::from_bounds(base_span.start, field_span.end());
            let symbol = self.intern_text(&qualified, name_span);
            return Ok(self.parse_struct_literal(symbol, name_span));
        }
        // `view.toolbar { … }` is a method call whose arguments are all content.
        // Without the parentheses there is nothing else for the `{` to be: a
        // field read cannot take a block, and the qualified-literal reading
        // above already claimed the case where the base is a bare name path. The
        // call is built with no arguments and the postfix loop attaches the
        // block to it, exactly as it does for `f(x) { … }`.
        //
        // A `{` that opens an enclosing body is excluded by `at_trailing_block`,
        // which is gated on the same `no_struct_literal` flag that stops
        // `if p.ready { … }` from reading its body as content.
        if self.at_trailing_block() {
            let span = Span::from_bounds(base_span.start, field_span.end());
            return Ok(self.tree.add_expr(Expr::MethodCall {
                receiver: base,
                method: field,
                method_span: field_span,
                args: Vec::new(),
                children: Vec::new(),
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

    /// Reconstructs the dotted spelling of a pure name path (`A`, `A.B`,
    /// `A.B.C`), or `None` when the base is anything else.
    ///
    /// A module-qualified struct literal's qualifier is one of these paths, and
    /// rebuilding it from the leaf spans drops any whitespace between segments —
    /// the same way [`Parser::parse_type_ref`] assembles a qualified type name a
    /// token at a time.
    fn name_path_text(&self, base: ExprId) -> Option<String> {
        match self.tree.expr(base) {
            Expr::Name { span, .. } => Some(self.text_of(*span).to_owned()),
            Expr::Field {
                base, field_span, ..
            } => {
                let prefix = self.name_path_text(*base)?;
                Some(format!("{prefix}.{}", self.text_of(*field_span)))
            }
            _ => None,
        }
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
                // The lexer already reported an unknown escape (`KLEX003`);
                // no guessed-at text may stand in for the literal.
                match decode_string_literal(self.text_of(token.span)) {
                    Ok(value) => self.tree.add_expr(Expr::Str {
                        value,
                        span: token.span,
                    }),
                    Err(_) => self.tree.add_expr(Expr::Error { span: token.span }),
                }
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

    /// Parses an integer literal, decimal or hexadecimal.
    ///
    /// A hex literal is a **bit pattern**, so it is read as 64 unsigned bits and
    /// reinterpreted: `0xffffffffffffffff` is `-1`, the same value C writes that
    /// way, and a mask does not have to be spelled as a negative decimal. A
    /// decimal literal is a *number* and keeps its own range — `9223372036854775808`
    /// is out of range whichever way it is written.
    fn parse_int(&mut self, span: Span) -> ExprId {
        self.bump();
        let text = self.text_of(span);
        let parsed = match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
            Some(digits) => u64::from_str_radix(digits, 16).map(|bits| bits as i64),
            None => text.parse::<i64>(),
        };
        let value = match parsed {
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
        let value = match text.parse::<f64>() {
            Ok(value) if value.is_finite() => value,
            // A digit-run the lexer accepts cannot fail to parse today, but a
            // literal that overflows to infinity or stops parsing must not
            // become a silently wrong constant.
            _ => {
                self.error(span, "KPAR070", "float literal is out of range");
                0.0
            }
        };
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
            Some(self.parse_positional_call_args())
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
}

/// The operator and left binding power for a token that opens a binary
/// expression, or `None` when it is not a binary operator.
/// The binding power of `is` and `as`: above the comparisons (`a is T == b`
/// tests first) and below the shifts (`a << 1 as T` shifts first).
const TYPE_OPERATOR_BP: u8 = 8;

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
        TokenKind::LtLt => (BinaryOp::Shl, 9),
        TokenKind::GtGt => (BinaryOp::Shr, 9),
        TokenKind::Plus => (BinaryOp::Add, 10),
        TokenKind::Minus => (BinaryOp::Sub, 10),
        TokenKind::Star => (BinaryOp::Mul, 11),
        TokenKind::Slash => (BinaryOp::Div, 11),
        TokenKind::Percent => (BinaryOp::Rem, 11),
        _ => return None,
    };
    Some(pair)
}
