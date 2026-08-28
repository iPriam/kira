use super::*;

impl Parser<'_> {
    pub(super) fn parse_name_or_call(&mut self, name_span: Span) -> ExprId {
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
        // `nativeRecover` predates user-written generic calls and has a
        // dedicated type argument even when its list is malformed. Keep that
        // recovery path: otherwise `nativeRecover<T(raw)` is reinterpreted as
        // a comparison and loses the useful unclosed-list diagnostic.
        let type_args = if (name == "nativeRecover" && self.at(TokenKind::Lt))
            || self.at_explicit_call_type_args()
        {
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

    /// Distinguishes explicit generic call arguments from the less-than
    /// operator. A name is parsed before the precedence climber knows whether
    /// the following `<` is an operator, so consuming every `<` as a type list
    /// would turn `n < other` into a malformed call. Explicit arguments are
    /// unambiguous when their balanced list is followed by a call parenthesis
    /// or a construction/content brace.
    fn at_explicit_call_type_args(&self) -> bool {
        if !self.at(TokenKind::Lt) {
            return false;
        }
        let mut depth = 0i32;
        let mut offset = 0usize;
        loop {
            let kind = self.peek(offset).kind;
            match kind {
                TokenKind::Lt => depth += 1,
                TokenKind::Gt => {
                    depth -= 1;
                    if depth == 0 {
                        return matches!(
                            self.peek(offset + 1).kind,
                            TokenKind::LParen | TokenKind::LBrace
                        );
                    }
                    if depth < 0 {
                        return false;
                    }
                }
                TokenKind::GtGt => {
                    depth -= 2;
                    if depth <= 0 {
                        return depth == 0
                            && matches!(
                                self.peek(offset + 1).kind,
                                TokenKind::LParen | TokenKind::LBrace
                            );
                    }
                }
                TokenKind::Eof => return false,
                TokenKind::Identifier
                | TokenKind::Dot
                | TokenKind::Comma
                | TokenKind::LBracket
                | TokenKind::RBracket
                | TokenKind::LParen
                | TokenKind::RParen
                | TokenKind::Colon
                | TokenKind::Arrow => {}
                // A type argument list contains no expressions. Stopping at
                // the first token outside the type grammar is what prevents a
                // later comparison in the same function from being mistaken
                // for the close of this list.
                _ => return false,
            }
            offset += 1;
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

    pub(super) fn parse_call_args(&mut self) -> Vec<CallArg> {
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
    pub(super) fn parse_positional_call_args(&mut self) -> Vec<ExprId> {
        let args = self.parse_call_args();
        args.into_iter().map(|arg| arg.value).collect()
    }

    pub(super) fn parse_paren(&mut self) -> ExprId {
        self.bump(); // `(`
        let inner = self.with_struct_literals(|parser| parser.parse_expr());
        self.expect(TokenKind::RParen);
        inner
    }
}
