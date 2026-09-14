//! The multi-arm branch statement: `match`.
//!
//! An arm's head is a *pattern* naming one variant of the subject's enum,
//! optionally binding its payload — which is what gives `match` something to be
//! exhaustive over, and what a chain of `==` comparisons could never have.
//!
//! The subject suppresses struct literals where a brace follows an expression,
//! the rule a `while` condition follows: the brace opens a block, not a
//! literal.

use kira_source::Span;
use kira_syntax_model::TokenKind;
use kira_syntax_model::ast::{Block, Expr, MatchArm, MatchBinding, MatchPattern, Stmt, StmtId};

use crate::Parser;

impl Parser<'_> {
    /// Parses `match <subject> { <Variant>[(<binding>)] -> <arm> … }`.
    ///
    /// An arm's body is written either as a block (`Red -> { … }`) or as a
    /// single statement (`Red -> return 1`), and both become a [`Block`] here —
    /// the difference is spelling, not meaning, so nothing below the parser
    /// learns which was written. A trailing `;` after a single-statement arm is
    /// accepted and carries no meaning; arms are otherwise separated by nothing
    /// but their own extent.
    ///
    /// The subject suppresses struct literals for the reason a `while`
    /// subject does: the brace that follows it opens the match body.
    ///
    /// Only the *shape* is checked here. Whether `Red` names a variant of the
    /// subject's enum, whether every variant is covered, and whether one is
    /// covered twice are all questions about types, so analysis asks them.
    pub(super) fn parse_match(&mut self) -> StmtId {
        let start = self.current().span;
        self.bump(); // `match`
        let subject = self.without_struct_literals(|parser| parser.parse_expr());
        self.expect(TokenKind::LBrace);

        let mut arms = Vec::new();
        while !self.at(TokenKind::RBrace) && !self.at_eof() {
            match self.parse_match_arm() {
                Some(arm) => arms.push(arm),
                None => self.recover_to_next_arm(),
            }
        }
        self.expect(TokenKind::RBrace);
        let span = Span::from_bounds(start.start, self.previous_end());
        self.tree.add_stmt(Stmt::Match {
            subject,
            arms,
            span,
        })
    }

    /// Parses `attempt { … } handle { <Variant>[(<binding>)] { … } … }`.
    ///
    /// `handle` is matched as an *identifier*, not a keyword: the reference
    /// lexes it as one, so `let handle = 1` has to keep working. That costs
    /// nothing here — nothing else may follow an `attempt` body.
    ///
    /// A handler arm is spelled like a `match` arm with the `->` removed, and
    /// becomes the same [`MatchArm`]. The arm's body is always a block: without
    /// an arrow there is no way to tell a lone-statement arm from the next
    /// arm's head, so the reference does not offer one.
    pub(super) fn parse_attempt(&mut self) -> StmtId {
        let start = self.current().span;
        self.bump(); // `attempt`
        let body = self.parse_block();

        let mut handlers = Vec::new();
        if self.at_word("handle") {
            self.bump(); // `handle`
            self.expect(TokenKind::LBrace);
            while !self.at(TokenKind::RBrace) && !self.at_eof() {
                match self.parse_handler_arm() {
                    Some(arm) => handlers.push(arm),
                    None => self.recover_to_next_arm(),
                }
            }
            self.expect(TokenKind::RBrace);
        } else {
            self.error(
                self.current().span,
                "KPAR016",
                "expected `handle` after an `attempt` body",
            );
        }

        let span = Span::from_bounds(start.start, self.previous_end());
        self.tree.add_stmt(Stmt::Attempt {
            body,
            handlers,
            span,
        })
    }

    /// Parses one `handle` arm, or reports and returns `None` when the head is
    /// not a variant name or `else`.
    pub(super) fn parse_handler_arm(&mut self) -> Option<MatchArm> {
        let start = self.current().span;
        if self.at(TokenKind::Else) {
            let span = self.current().span;
            self.bump();
            let body = self.parse_block();
            return Some(MatchArm {
                patterns: vec![MatchPattern::Wildcard { span }],
                body,
                span: Span::from_bounds(start.start, self.previous_end()),
            });
        }
        if !self.at(TokenKind::Identifier) {
            self.error(
                start,
                "KPAR017",
                "expected a failure variant name to start a `handle` arm",
            );
            return None;
        }
        let variant_span = self.current().span;
        let variant = self.intern_span(variant_span);
        self.bump();
        let binding = self.parse_payload_binding();
        let body = self.parse_block();
        let span = Span::from_bounds(start.start, self.previous_end());
        Some(MatchArm {
            patterns: vec![MatchPattern::Variant {
                variant,
                variant_span,
                binding,
            }],
            body,
            span,
        })
    }

    /// Parses one `match` arm, or reports and returns `None` when the head is
    /// not a pattern.
    ///
    /// An arm head is one or more patterns separated by `,` (an alternation),
    /// then `->` and the body. Each pattern is an enum variant, a scalar
    /// literal, a half-open range, or the `else` catch-all.
    pub(crate) fn parse_match_arm(&mut self) -> Option<MatchArm> {
        let start = self.current().span;
        let mut patterns = vec![self.parse_match_pattern()?];
        while self.eat(TokenKind::Comma) {
            patterns.push(self.parse_match_pattern()?);
        }
        self.expect(TokenKind::Arrow);
        let body = self.parse_match_arm_body();
        let span = Span::from_bounds(start.start, self.previous_end());
        Some(MatchArm {
            patterns,
            body,
            span,
        })
    }

    /// Parses one expression-valued match arm.
    ///
    /// Braced arms keep the normal block grammar. An unbraced arm is parsed as
    /// an expression rather than a statement so `-> match ...` stays an
    /// [`Expr::Match`] and can itself produce the selected value.
    pub(crate) fn parse_match_expression_arm(&mut self) -> Option<MatchArm> {
        let start = self.current().span;
        let mut patterns = vec![self.parse_match_pattern()?];
        while self.eat(TokenKind::Comma) {
            patterns.push(self.parse_match_pattern()?);
        }
        self.expect(TokenKind::Arrow);
        let body = if self.at(TokenKind::LBrace) {
            self.parse_block()
        } else {
            let expr_start = self.current().span;
            let expr = self.parse_expr();
            let span = Span::from_bounds(expr_start.start, self.previous_end());
            let stmt = self.tree.add_stmt(Stmt::Expr { expr, span });
            Block {
                stmts: vec![stmt],
                span,
            }
        };
        let span = Span::from_bounds(start.start, self.previous_end());
        Some(MatchArm {
            patterns,
            body,
            span,
        })
    }

    /// Parses one pattern in a `match` arm head.
    pub(super) fn parse_match_pattern(&mut self) -> Option<MatchPattern> {
        let start = self.current().span;
        // `else`: the catch-all.
        if self.at(TokenKind::Else) {
            self.bump();
            return Some(MatchPattern::Wildcard { span: start });
        }
        // A variant name, optionally binding its payload.
        if self.at(TokenKind::Identifier) {
            let variant_span = self.current().span;
            let variant = self.intern_span(variant_span);
            self.bump();
            let binding = self.parse_payload_binding();
            return Some(MatchPattern::Variant {
                variant,
                variant_span,
                binding,
            });
        }
        // A scalar literal, or the lower bound of a range.
        if self.starts_pattern_literal() {
            let first = self.parse_pattern_literal();
            if self.eat(TokenKind::DotDot) {
                let end = self.parse_pattern_literal();
                let span = Span::from_bounds(start.start, self.previous_end());
                return Some(MatchPattern::Range {
                    start: first,
                    end,
                    span,
                });
            }
            let span = Span::from_bounds(start.start, self.previous_end());
            return Some(MatchPattern::Value { expr: first, span });
        }
        self.error(
            start,
            "KPAR014",
            "expected an enum variant, a literal, a range, or `else` to start a `match` arm",
        );
        None
    }

    /// Whether the current token can begin a scalar literal pattern.
    fn starts_pattern_literal(&self) -> bool {
        matches!(
            self.current_kind(),
            TokenKind::IntLiteral
                | TokenKind::FloatLiteral
                | TokenKind::StringLiteral
                | TokenKind::True
                | TokenKind::False
                | TokenKind::Minus
        )
    }

    /// Parses one scalar literal in a pattern: an integer (with an optional
    /// leading `-`), a float, a string, or a boolean.
    fn parse_pattern_literal(&mut self) -> kira_syntax_model::ast::ExprId {
        let start = self.current().span;
        if self.at(TokenKind::Minus) {
            self.bump();
            let operand = self.parse_literal_primary();
            let span = Span::from_bounds(start.start, self.previous_end());
            return self.tree.add_expr(Expr::Unary {
                op: kira_syntax_model::ast::UnaryOp::Neg,
                operand,
                span,
            });
        }
        self.parse_literal_primary()
    }

    /// Parses a single literal token as an expression: integer, float, string,
    /// or boolean. Anything else is an error placeholder.
    fn parse_literal_primary(&mut self) -> kira_syntax_model::ast::ExprId {
        let span = self.current().span;
        match self.current_kind() {
            TokenKind::IntLiteral => self.parse_int(span),
            TokenKind::FloatLiteral => self.parse_float(span),
            TokenKind::StringLiteral => self.parse_string_literal(span),
            TokenKind::True => {
                self.bump();
                self.tree.add_expr(Expr::Bool { value: true, span })
            }
            TokenKind::False => {
                self.bump();
                self.tree.add_expr(Expr::Bool { value: false, span })
            }
            _ => {
                self.error(span, "KPAR020", "expected a literal in a pattern");
                self.bump();
                self.error_expr(span)
            }
        }
    }

    /// Parses an optional `(<name>)` payload binding after a variant name.
    ///
    /// Shared by `match` and `handle` arms, which spell it identically:
    /// `Label(text)` binds the variant's payload, `Red` binds nothing.
    fn parse_payload_binding(&mut self) -> Option<MatchBinding> {
        if !self.eat(TokenKind::LParen) {
            return None;
        }
        let binding = if self.at(TokenKind::Identifier) {
            let span = self.current().span;
            let name = self.intern_span(span);
            self.bump();
            Some(MatchBinding { name, span })
        } else {
            self.error(
                self.current().span,
                "KPAR015",
                "expected a name to bind the variant's payload to",
            );
            None
        };
        self.expect(TokenKind::RParen);
        binding
    }

    /// Parses an arm's body: a block as written, or a lone statement wrapped in
    /// one.
    fn parse_match_arm_body(&mut self) -> Block {
        if self.at(TokenKind::LBrace) {
            return self.parse_block();
        }
        let start = self.current().span;
        let stmts = self.parse_stmt().into_iter().collect();
        Block {
            stmts,
            span: Span::from_bounds(start.start, self.previous_end()),
        }
    }

    /// Skips to where the next arm could start, so one bad arm costs only
    /// itself.
    pub(crate) fn recover_to_next_arm(&mut self) {
        while !self.at_eof() && !self.at(TokenKind::RBrace) {
            if self.at(TokenKind::LBrace) {
                self.skip_balanced(TokenKind::LBrace, TokenKind::RBrace);
                return;
            }
            self.bump();
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::parse;
    use kira_source::SourceId;
    use kira_syntax_model::ast::{Item, MatchBinding, MatchPattern, Stmt};

    /// The variant name and binding of a single-variant arm, panicking when the
    /// arm is not one plain variant pattern.
    fn variant_arm(pattern: &MatchPattern) -> (kira_core::Symbol, Option<MatchBinding>) {
        match pattern {
            MatchPattern::Variant {
                variant, binding, ..
            } => (*variant, *binding),
            other => panic!("expected a variant pattern, got {other:?}"),
        }
    }

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
    /// The brace after a subject opens a block, so it may not read as a struct
    /// literal — otherwise the arm's body is swallowed.
    #[test]
    fn a_match_parses_both_arm_shapes_and_a_payload_binding() {
        let (tree, stmts) =
            body("function f() { match e { Red -> return 1 Label(text) -> { out = text } } }");
        let Stmt::Match { arms, .. } = &stmts[0] else {
            panic!("expected a match, got {:?}", stmts[0]);
        };
        assert_eq!(arms.len(), 2);

        // `Red -> return 1;` — a lone statement becomes a one-statement block,
        // and the `;` is a separator rather than a second statement.
        let (_, binding0) = variant_arm(&arms[0].patterns[0]);
        assert!(binding0.is_none());
        assert_eq!(arms[0].body.stmts.len(), 1);
        assert!(matches!(
            tree.stmt(arms[0].body.stmts[0]),
            Stmt::Return { .. }
        ));

        // `Label(text) -> { … }` — the block as written, plus the binding.
        let (variant1, binding1) = variant_arm(&arms[1].patterns[0]);
        let binding = binding1.expect("the second arm binds a payload");
        assert_eq!(arms[1].body.stmts.len(), 1);
        assert!(matches!(
            tree.stmt(arms[1].body.stmts[0]),
            Stmt::Assign { .. }
        ));
        assert_ne!(
            binding.name, variant1,
            "the binding is its own name, not the variant's"
        );
    }

    #[test]
    fn an_expression_match_arm_keeps_a_nested_match_as_an_expression() {
        let result = parse(
            SourceId::new(0),
            "function f(stream: Int, direct: Bool) -> Int { return match stream <= 0 { true -> 0 false -> match stream { 1 -> match direct { true -> 10 false -> 11 } else -> 20 } } }",
        );
        assert!(
            result.diagnostics.is_empty(),
            "unexpected diagnostics: {:?}",
            result.diagnostics
        );
        let Item::Function(function) = &result.tree.items()[0] else {
            panic!("expected a function");
        };
        let Stmt::Return {
            value: Some(outer), ..
        } = result.tree.stmt(function.body.stmts[0])
        else {
            panic!("expected return of outer match");
        };
        let kira_syntax_model::ast::Expr::Match { arms, .. } = result.tree.expr(*outer) else {
            panic!("expected outer expression match");
        };
        let Stmt::Expr { expr: middle, .. } = result.tree.stmt(arms[1].body.stmts[0]) else {
            panic!("expected nested match to remain an expression statement");
        };
        let kira_syntax_model::ast::Expr::Match {
            arms: middle_arms, ..
        } = result.tree.expr(*middle)
        else {
            panic!("expected middle expression match");
        };
        let Stmt::Expr { expr: inner, .. } = result.tree.stmt(middle_arms[0].body.stmts[0]) else {
            panic!("expected inner match to remain an expression statement");
        };
        assert!(matches!(
            result.tree.expr(*inner),
            kira_syntax_model::ast::Expr::Match { .. }
        ));
    }

    #[test]
    fn a_match_parses_else_alternation_and_ranges() {
        let (_, stmts) = body(
            "function f() { match n { 1, 2, 3 -> return 1 0..10 -> return 2 else -> return 3 } }",
        );
        let Stmt::Match { arms, .. } = &stmts[0] else {
            panic!("expected a match, got {:?}", stmts[0]);
        };
        assert_eq!(arms.len(), 3);
        assert_eq!(
            arms[0].patterns.len(),
            3,
            "an alternation of three literals"
        );
        assert!(matches!(arms[0].patterns[0], MatchPattern::Value { .. }));
        assert!(matches!(arms[1].patterns[0], MatchPattern::Range { .. }));
        assert!(matches!(arms[2].patterns[0], MatchPattern::Wildcard { .. }));
    }

    /// The subject's brace opens the match body, not a struct literal — the
    /// same rule a `while` condition follows.
    #[test]
    fn a_match_subject_does_not_swallow_the_body_brace() {
        let (_, stmts) = body("function f() { match shape { Empty -> return 0 } }");
        let Stmt::Match { arms, .. } = &stmts[0] else {
            panic!("expected a match, got {:?}", stmts[0]);
        };
        assert_eq!(arms.len(), 1, "the brace opened the match, not a literal");
    }

    /// One unparseable arm head is reported and skipped; the arms around it
    /// survive. `@` can start neither a variant, a literal, a range, nor
    /// `else`, so it is the head that has no pattern.
    #[test]
    fn a_match_arm_without_a_pattern_is_reported_and_recovers() {
        let result = parse(
            SourceId::new(0),
            "function f() { match e { Red -> return 1 @ -> return 2 Blue -> return 3 } }",
        );
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.has_code("KPAR014")),
            "expected KPAR014, got {:?}",
            result.diagnostics
        );
        assert_eq!(result.tree.items().len(), 1, "the function still parses");
    }

    /// The shape of an `attempt`: a body, then arms with no arrow.
    #[test]
    fn an_attempt_parses_its_body_and_handler_arms() {
        let (tree, stmts) = body(
            "function f() { attempt { let v = try g() return v } \
             handle { TooSmall { return 0 } TooBig(reason) { return 1 } } }",
        );
        let Stmt::Attempt { body, handlers, .. } = &stmts[0] else {
            panic!("expected an attempt, got {:?}", stmts[0]);
        };
        assert_eq!(body.stmts.len(), 2);
        assert_eq!(handlers.len(), 2);

        // `TooSmall { … }` — no arrow, no binding.
        let (_, binding0) = variant_arm(&handlers[0].patterns[0]);
        assert!(binding0.is_none());
        assert_eq!(handlers[0].body.stmts.len(), 1);

        // `TooBig(reason) { … }` — the binding is its own name.
        let (variant1, binding1) = variant_arm(&handlers[1].patterns[0]);
        let binding = binding1.expect("the second arm binds a payload");
        assert_ne!(binding.name, variant1);

        // The `try` is the whole initializer of the `let`, which is the one
        // position analysis accepts it in.
        let Stmt::Let { init, .. } = tree.stmt(body.stmts[0]) else {
            panic!("expected a let");
        };
        assert!(matches!(
            tree.expr(*init),
            kira_syntax_model::ast::Expr::Try { .. }
        ));
    }

    /// `handle` is an identifier, not a keyword — the reference lexes it as one,
    /// so it has to stay usable as a name.
    #[test]
    fn handle_is_still_available_as_a_name() {
        let (_, stmts) = body("function f() { let handle = 1 return handle }");
        assert!(matches!(stmts[0], Stmt::Let { .. }));
    }

    /// An `attempt` body with no `handle` after it is reported, and the function
    /// still parses.
    #[test]
    fn an_attempt_without_a_handle_is_reported_and_recovers() {
        let result = parse(
            SourceId::new(0),
            "function f() { attempt { let v = try g() } }",
        );
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.has_code("KPAR016")),
            "expected KPAR016, got {:?}",
            result.diagnostics
        );
        assert_eq!(result.tree.items().len(), 1, "the function still parses");
    }

    /// One unparseable handler arm is reported and skipped; the arms around it
    /// survive.
    #[test]
    fn a_handler_arm_without_a_variant_name_is_reported_and_recovers() {
        let result = parse(
            SourceId::new(0),
            "function f() { attempt { let v = try g() } \
             handle { A { return 1 } 42 { return 2 } B { return 3 } } }",
        );
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.has_code("KPAR017")),
            "expected KPAR017, got {:?}",
            result.diagnostics
        );
        assert_eq!(result.tree.items().len(), 1, "the function still parses");
    }

    /// A `(` with no name inside is reported, and the arm still parses.
    #[test]
    fn a_payload_binding_without_a_name_is_reported() {
        let result = parse(
            SourceId::new(0),
            "function f() { match e { Label() -> return 1 } }",
        );
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.has_code("KPAR015")),
            "expected KPAR015, got {:?}",
            result.diagnostics
        );
    }
}
