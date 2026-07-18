//! The two multi-arm branch statements: `switch` and `match`.
//!
//! They share a silhouette and almost nothing else. A `switch` arm's head is an
//! *expression* compared with `==`; a `match` arm's head is a *pattern* naming
//! one variant of the subject's enum, optionally binding its payload. That is
//! why they are two constructs and not one with a flag — and why only `match`
//! has anything to be exhaustive over.
//!
//! Both suppress struct literals where a brace follows an expression, the rule
//! a `while` condition follows: the brace opens a block, not a literal.

use kira_source::Span;
use kira_syntax_model::TokenKind;
use kira_syntax_model::ast::{Block, MatchArm, MatchBinding, Stmt, StmtId, SwitchCase};

use crate::Parser;

impl Parser<'_> {
    /// Parses `switch <subject> { case <label> { … } … default { … } }`.
    ///
    /// Both the subject and each `case` label suppress struct literals, for the
    /// same reason a `while` condition does: the brace that follows either one
    /// opens a block — the switch body, or the arm's — so a literal read there
    /// would swallow it. `case Shape { … }` is the arm, not a `Shape` literal.
    ///
    /// `default` is optional and is not required to come last; a repeated one
    /// replaces the previous rather than being diagnosed, matching the language.
    pub(super) fn parse_switch(&mut self) -> StmtId {
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

    /// Parses `match <subject> { <Variant>[(<binding>)] -> <arm> … }`.
    ///
    /// An arm's body is written either as a block (`Red -> { … }`) or as a
    /// single statement (`Red -> return 1`), and both become a [`Block`] here —
    /// the difference is spelling, not meaning, so nothing below the parser
    /// learns which was written. A trailing `;` after a single-statement arm is
    /// accepted and carries no meaning; arms are otherwise separated by nothing
    /// but their own extent.
    ///
    /// The subject suppresses struct literals for the reason a `switch`
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

    /// Parses one `match` arm, or reports and returns `None` when the head is
    /// not a pattern.
    fn parse_match_arm(&mut self) -> Option<MatchArm> {
        let start = self.current().span;
        if !self.at(TokenKind::Identifier) {
            self.error(
                start,
                "KPAR014",
                "expected an enum variant name to start a `match` arm",
            );
            return None;
        }
        let variant_span = self.current().span;
        let variant = self.intern_span(variant_span);
        self.bump();

        // `Label(text)` binds the variant's payload; `Red` binds nothing.
        let binding = if self.eat(TokenKind::LParen) {
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
        } else {
            None
        };

        self.expect(TokenKind::Arrow);
        let body = self.parse_match_arm_body();
        let span = Span::from_bounds(start.start, self.previous_end());
        Some(MatchArm {
            variant,
            variant_span,
            binding,
            body,
            span,
        })
    }

    /// Parses an arm's body: a block as written, or a lone statement wrapped in
    /// one.
    fn parse_match_arm_body(&mut self) -> Block {
        if self.at(TokenKind::LBrace) {
            return self.parse_block();
        }
        let start = self.current().span;
        let stmts = self.parse_stmt().into_iter().collect();
        // The `;` in `Red -> return 1;` is a separator, not a statement.
        self.eat(TokenKind::Semicolon);
        Block {
            stmts,
            span: Span::from_bounds(start.start, self.previous_end()),
        }
    }

    /// Skips to where the next arm could start, so one bad arm costs only
    /// itself.
    fn recover_to_next_arm(&mut self) {
        while !self.at_eof() && !self.at(TokenKind::RBrace) {
            if self.at(TokenKind::LBrace) {
                self.skip_balanced(TokenKind::LBrace, TokenKind::RBrace);
                return;
            }
            if self.at(TokenKind::Semicolon) {
                self.bump();
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

    /// Both arm spellings, and a payload binding, in one match.
    #[test]
    fn a_match_parses_both_arm_shapes_and_a_payload_binding() {
        let (tree, stmts) =
            body("function f() { match e { Red -> return 1; Label(text) -> { out = text } } }");
        let Stmt::Match { arms, .. } = &stmts[0] else {
            panic!("expected a match, got {:?}", stmts[0]);
        };
        assert_eq!(arms.len(), 2);

        // `Red -> return 1;` — a lone statement becomes a one-statement block,
        // and the `;` is a separator rather than a second statement.
        assert!(arms[0].binding.is_none());
        assert_eq!(arms[0].body.stmts.len(), 1);
        assert!(matches!(
            tree.stmt(arms[0].body.stmts[0]),
            Stmt::Return { .. }
        ));

        // `Label(text) -> { … }` — the block as written, plus the binding.
        let binding = arms[1].binding.expect("the second arm binds a payload");
        assert_eq!(arms[1].body.stmts.len(), 1);
        assert!(matches!(
            tree.stmt(arms[1].body.stmts[0]),
            Stmt::Assign { .. }
        ));
        assert_ne!(
            binding.name, arms[1].variant,
            "the binding is its own name, not the variant's"
        );
    }

    /// The subject's brace opens the match body, not a struct literal — the
    /// same rule a `while` condition follows.
    #[test]
    fn a_match_subject_does_not_swallow_the_body_brace() {
        let (_, stmts) = body("function f() { match shape { Empty -> return 0; } }");
        let Stmt::Match { arms, .. } = &stmts[0] else {
            panic!("expected a match, got {:?}", stmts[0]);
        };
        assert_eq!(arms.len(), 1, "the brace opened the match, not a literal");
    }

    /// One unparseable arm is reported and skipped; the arms around it survive.
    #[test]
    fn a_match_arm_without_a_variant_name_is_reported_and_recovers() {
        let result = parse(
            SourceId::new(0),
            "function f() { match e { Red -> return 1; 42 -> return 2; Blue -> return 3; } }",
        );
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == Some("KPAR014")),
            "expected KPAR014, got {:?}",
            result.diagnostics
        );
        assert_eq!(result.tree.items.len(), 1, "the function still parses");
    }

    /// A `(` with no name inside is reported, and the arm still parses.
    #[test]
    fn a_payload_binding_without_a_name_is_reported() {
        let result = parse(
            SourceId::new(0),
            "function f() { match e { Label() -> return 1; } }",
        );
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == Some("KPAR015")),
            "expected KPAR015, got {:?}",
            result.diagnostics
        );
    }
}
