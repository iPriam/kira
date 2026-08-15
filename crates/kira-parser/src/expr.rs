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
mod closures;
mod content;

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
        // `try` binds like a prefix operator so `try f(n)` takes the whole
        // call. Whether this position is one where a `try` is allowed at all is
        // a question for analysis, not the parser.
        if self.at(TokenKind::Try) {
            let start = self.current().span;
            self.bump(); // `try`
            let value = self.parse_unary();
            let value_span = self.tree.expr(value).span();
            let span = Span::from_bounds(start.start, value_span.end());
            return self.tree.add_expr(Expr::Try { value, span });
        }
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
        {
            let qualified = format!("{prefix}.{}", self.text_of(field_span));
            let name_span = Span::from_bounds(base_span.start, field_span.end());
            let symbol = self.intern_text(&qualified, name_span);
            return Ok(self.parse_struct_literal(symbol, name_span));
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

    fn parse_name_or_call(&mut self, name_span: Span) -> ExprId {
        let name = self.text_of(name_span).to_owned();
        let symbol = self.intern_span(name_span);
        self.bump();
        // `Task { work(1, 2) }` spawns a deferred task. Its block holds one
        // expression rather than a child list, so it is recognized before the
        // content-block and struct-literal arms, both of which would read the
        // body as something it is not.
        if name == "Task" && self.at(TokenKind::LBrace) {
            return self.parse_task_spawn(name_span);
        }
        let type_args = if name == "nativeRecover" && self.at(TokenKind::Lt) {
            self.parse_call_type_args()
        } else {
            Vec::new()
        };
        if self.at(TokenKind::LParen) {
            let args = self.parse_call_args();
            let span = Span::from_bounds(name_span.start, self.previous_end());
            self.tree.add_expr(Expr::Call {
                callee: symbol,
                callee_span: name_span,
                braced: false,
                type_args,
                args,
                children: Vec::new(),
                trailing_closure: None,
                span,
            })
        } else if self.at_bare_construct_block() || self.at_content_block() {
            // `HGroup { child child }` — a construction that passes children
            // with no argument list — and the empty/`let` forms ambiguous with a
            // struct literal. One block holds both children and overrides, so
            // `HGroup { child spacing: 8 }` names the enclosing construction's
            // input the way a construction with an argument list would.
            let mut args = Vec::new();
            let brace = self.parse_deferred_brace(&mut args);
            let span = Span::from_bounds(name_span.start, self.previous_end());
            self.tree.add_expr(Expr::Call {
                callee: symbol,
                callee_span: name_span,
                braced: true,
                type_args,
                args,
                children: brace.children,
                trailing_closure: brace.closure,
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

    /// Parses `Task { expression }`, with `Task` already consumed.
    ///
    /// The block holds exactly one expression. A second one is refused here
    /// rather than silently dropped: a task body that ran only its first
    /// statement would be a wrong answer, not a syntax error.
    fn parse_task_spawn(&mut self, name_span: Span) -> ExprId {
        let open = self.current().span;
        self.bump(); // `{`
        let body = if self.at(TokenKind::RBrace) {
            self.error(
                open,
                "KPAR060",
                "a `Task { … }` block holds the one expression the task defers",
            );
            self.tree.add_expr(Expr::Error { span: open })
        } else {
            self.parse_expr()
        };
        while self.eat(TokenKind::Semicolon) {}
        if !self.at(TokenKind::RBrace) && !self.at_eof() {
            self.error(
                self.current().span,
                "KPAR060",
                "a `Task { … }` block holds one expression; write a function and \
                 spawn a call to it for anything longer",
            );
            self.skip_to_close_brace();
        }
        self.expect(TokenKind::RBrace);
        let span = Span::from_bounds(name_span.start, self.previous_end());
        self.tree.add_expr(Expr::TaskSpawn { body, span })
    }

    /// Consumes tokens up to the `}` closing the current block.
    fn skip_to_close_brace(&mut self) {
        let mut depth = 0usize;
        while !self.at_eof() {
            match self.current_kind() {
                TokenKind::LBrace => depth += 1,
                TokenKind::RBrace if depth == 0 => return,
                TokenKind::RBrace => depth -= 1,
                _ => {}
            }
            self.bump();
        }
    }

    fn parse_call_args(&mut self) -> Vec<CallArg> {
        let mut args = Vec::new();
        self.bump(); // `(`
        self.with_struct_literals(|parser| {
            while !parser.at(TokenKind::RParen) && !parser.at_eof() {
                let before = parser.pos;
                args.push(parser.parse_call_arg());
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

    /// Parses one call argument, with an optional `label:` / `label =` binder.
    ///
    /// A leading identifier is a label only when a binder follows it, so `f(x)`
    /// and `f(x + 1)` keep `x` as the start of an ordinary expression. Both
    /// binders are accepted — `=` is canonical, `:` stays valid for the
    /// transition window — mirroring a struct literal's field, and they
    /// normalize to one node. What a label binds to is a question for analysis,
    /// which holds the callee's parameters; the parser only records the name.
    fn parse_call_arg(&mut self) -> CallArg {
        let (label, label_span) = if self.at(TokenKind::Identifier)
            && matches!(self.peek(1).kind, TokenKind::Colon | TokenKind::Equals)
        {
            let span = self.current().span;
            let symbol = self.intern_span(span);
            self.bump(); // label
            self.bump(); // binder
            (Some(symbol), Some(span))
        } else {
            (None, None)
        };
        let value = self.parse_expr();
        let start =
            label_span.map_or_else(|| self.tree.expr(value).span().start, |span| span.start);
        let span = Span::from_bounds(start, self.previous_end());
        CallArg {
            label,
            label_span,
            value,
            span,
        }
    }

    /// Wraps a value expression as a positional (unlabeled) call argument.
    ///
    /// A trailing closure attaches this way: `f { … }` grows the call by one
    /// argument that carries no label.
    pub(crate) fn positional_arg(&self, value: ExprId) -> CallArg {
        let span = self.tree.expr(value).span();
        CallArg {
            label: None,
            label_span: None,
            value,
            span,
        }
    }

    /// Parses a `(...)` argument list for an enum variant payload.
    ///
    /// The payload still binds by position — the label is only the readable
    /// name at the call site — so the syntax tree keeps the same value-only
    /// representation used by the enum analyzer.
    fn parse_positional_call_args(&mut self) -> Vec<ExprId> {
        let args = self.parse_call_args();
        args.into_iter().map(|arg| arg.value).collect()
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
