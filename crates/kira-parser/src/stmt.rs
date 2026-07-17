//! Statement parsing for the recursive-descent parser.
//!
//! Recovery boundary: a statement that cannot be parsed becomes a
//! [`Stmt::Error`] and the cursor resynchronizes at the next `;`, `}`, or
//! statement-starting keyword, so one bad statement never derails the block.

use kira_core::Symbol;
use kira_source::Span;
use kira_syntax_model::TokenKind;
use kira_syntax_model::ast::{Block, ForIterable, Stmt, StmtId, SwitchCase};

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
            TokenKind::Switch => Some(self.parse_switch()),
            TokenKind::Match => Some(self.parse_unsupported_stmt()),
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

    /// Parses `switch <subject> { case <label> { … } … default { … } }`.
    ///
    /// Both the subject and each `case` label suppress struct literals, for the
    /// same reason a `while` condition does: the brace that follows either one
    /// opens a block — the switch body, or the arm's — so a literal read there
    /// would swallow it. `case Shape { … }` is the arm, not a `Shape` literal.
    ///
    /// `default` is optional and is not required to come last; a repeated one
    /// replaces the previous rather than being diagnosed, matching the language.
    fn parse_switch(&mut self) -> StmtId {
        let start = self.current().span;
        self.bump(); // `switch`
        let subject = self.without_struct_literals(|parser| parser.parse_expr());
        self.expect(TokenKind::LBrace);

        let mut cases = Vec::new();
        let mut default_block = None;
        while !self.at(TokenKind::RBrace) && !self.at_eof() {
            match self.current_kind() {
                TokenKind::Case => {
                    let case_start = self.current().span;
                    self.bump(); // `case`
                    let label = self.without_struct_literals(|parser| parser.parse_expr());
                    // The binder is optional: `case 1 { … }` and
                    // `case 1: { … }` are the same arm.
                    self.eat(TokenKind::Colon);
                    let body = self.parse_block();
                    let span = Span::from_bounds(case_start.start, self.previous_end());
                    cases.push(SwitchCase { label, body, span });
                }
                TokenKind::Default => {
                    self.bump(); // `default`
                    self.eat(TokenKind::Colon);
                    default_block = Some(self.parse_block());
                }
                _ => {
                    self.error(
                        self.current().span,
                        "KPAR013",
                        "expected `case` or `default` in a switch body",
                    );
                    // Resynchronize: skip to the next arm or the closing brace,
                    // so one bad arm does not cost the rest of the switch.
                    while !self.at_eof()
                        && !self.at(TokenKind::Case)
                        && !self.at(TokenKind::Default)
                        && !self.at(TokenKind::RBrace)
                    {
                        if self.at(TokenKind::LBrace) {
                            self.skip_balanced(TokenKind::LBrace, TokenKind::RBrace);
                        } else {
                            self.bump();
                        }
                    }
                }
            }
        }
        self.expect(TokenKind::RBrace);
        let span = Span::from_bounds(start.start, self.previous_end());
        self.tree.add_stmt(Stmt::Switch {
            subject,
            cases,
            default_block,
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
                // `return [1, 2, 3]` — an array literal is a value like any
                // other, so `[` starts an expression.
                | TokenKind::LBracket
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
    fn a_switch_parses_its_arms_and_optional_default() {
        let (_, stmts) = body("function f() { switch n { case 0 { } case 1 { } default { } } }");
        match &stmts[0] {
            Stmt::Switch {
                cases,
                default_block,
                ..
            } => {
                assert_eq!(cases.len(), 2);
                assert!(default_block.is_some());
            }
            other => panic!("expected a `switch`, got {other:?}"),
        }

        // `default` is optional.
        let (_, stmts) = body("function f() { switch n { case 0 { } } }");
        match &stmts[0] {
            Stmt::Switch { default_block, .. } => assert!(default_block.is_none()),
            other => panic!("expected a `switch`, got {other:?}"),
        }
    }

    /// The binder after a label is optional: `case 0 { … }` and `case 0: { … }`
    /// are the same arm. Only the spans differ, since the `:` moves what
    /// follows it, so the shape is what is compared.
    #[test]
    fn a_case_label_accepts_an_optional_colon() {
        for source in [
            "function f() { switch n { case 0 { let x = 1 } } }",
            "function f() { switch n { case 0: { let x = 1 } } }",
        ] {
            let (tree, stmts) = body(source);
            match &stmts[0] {
                Stmt::Switch {
                    cases,
                    default_block,
                    ..
                } => {
                    assert_eq!(cases.len(), 1, "{source}");
                    assert!(default_block.is_none(), "{source}");
                    assert_eq!(cases[0].body.stmts.len(), 1, "{source}");
                    assert!(
                        matches!(
                            tree.expr(cases[0].label),
                            kira_syntax_model::ast::Expr::Int { value: 0, .. }
                        ),
                        "{source}"
                    );
                }
                other => panic!("expected a `switch`, got {other:?}"),
            }
        }
    }

    /// The brace after a subject or a label opens a block, so neither may read
    /// as a struct literal — otherwise the arm's body is swallowed.
    #[test]
    fn a_switch_subject_and_label_do_not_swallow_their_braces() {
        let (_, stmts) = body("function f() { switch subject { case label { let x = 1 } } }");
        match &stmts[0] {
            Stmt::Switch { cases, .. } => {
                assert_eq!(cases.len(), 1);
                assert_eq!(cases[0].body.stmts.len(), 1, "the brace is the arm body");
            }
            other => panic!("expected a `switch`, got {other:?}"),
        }
    }

    /// `default` is not required to come last, and a repeated one replaces the
    /// previous rather than being diagnosed.
    #[test]
    fn a_switch_accepts_default_in_any_position() {
        let (_, stmts) = body("function f() { switch n { default { } case 0 { } } }");
        match &stmts[0] {
            Stmt::Switch {
                cases,
                default_block,
                ..
            } => {
                assert_eq!(cases.len(), 1);
                assert!(default_block.is_some());
            }
            other => panic!("expected a `switch`, got {other:?}"),
        }
    }

    /// A statement where an arm belongs is reported, and the arms around it
    /// still parse.
    #[test]
    fn a_switch_body_without_an_arm_keyword_is_reported_and_recovers() {
        let result = parse(
            SourceId::new(0),
            "function f() { switch n { print(1) case 0 { } } }",
        );
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == Some("KPAR013"))
        );
        assert_eq!(result.tree.items.len(), 1, "the function still parses");
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

    /// `for i in 0 { }` used to be a parse error, because `..` was mandatory.
    /// Now that `for x in xs` exists, the parser cannot demand `..` — so this
    /// parses cleanly as an `Each` over `0`, and reporting that an `Int` is not
    /// iterable belongs to analysis, which is where the type is known.
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
        assert_eq!(result.tree.items.len(), 1, "the function is still there");
    }
}
