//! The member level of a construct body: one `let`, `function`, requirement,
//! or slot at a time.
//!
//! Split out of [`super`] on the file-size ladder, along the seam that was
//! already there. [`super`] decides *which declaration* is being parsed and
//! walks its body; everything here answers *what this one member is*. The two
//! never need each other's state — a member parser sees only the cursor and the
//! [`ConstructBody`] it appends to.
//!
//! Every requirement spelling lands here, and they converge on purpose: an
//! `@Required function` and an entry of a `requires { … }` section build the
//! same bodyless member, so nothing downstream can tell which one was written.

use kira_core::Symbol;
use kira_runtime_abi::Execution;
use kira_source::Span;
use kira_syntax_model::TokenKind;
use kira_syntax_model::ast::{
    Block, ConstructField, ConstructMethod, DeferredConstruct, Function, Param, Stmt, TypeRefId,
};

use super::ConstructBody;
use crate::Parser;

impl Parser<'_> {
    /// Parses an `@Name`-annotated construct member.
    pub(super) fn parse_annotated_construct_member(&mut self, body: &mut ConstructBody) {
        let at_span = self.current().span;
        self.bump(); // `@`
        if !self.at(TokenKind::Identifier) {
            self.error(
                self.current().span,
                "KPAR003",
                "expected an annotation name after `@`",
            );
            return;
        }
        let name_span = self.current().span;
        let name = self.text_of(name_span).to_owned();
        self.bump();
        match name.as_str() {
            // `@Required let name: Any` — a member every backed declaration must
            // provide. This one executes: it is a stored field.
            //
            // `@Required function name(…) -> T` — a bodyless signature every
            // backed declaration must implement. Both are Construct 2.0's
            // top-level member spelling of a requirement.
            "Required" => match self.current_kind() {
                TokenKind::Let => self.parse_construct_let_required(body),
                TokenKind::Function => self.parse_construct_required_function(body),
                _ => self.error(
                    self.current().span,
                    "KPAR060",
                    "`@Required` annotates a `let` or `function` member",
                ),
            },
            // `@Content let x: Any` — the compat spelling of a `some Any` child
            // slot. It parses to a real slot field: analysis fills it from a
            // construction's trailing children, and executes it when its declared
            // type is concrete.
            "Content" => {
                if self.at(TokenKind::Let) {
                    self.parse_construct_content_slot(body);
                } else {
                    self.error(
                        self.current().span,
                        "KPAR063",
                        "`@Content` annotates a `let` child-slot member",
                    );
                }
            }
            "Consuming" => {
                if self.at(TokenKind::Function) {
                    if let Some(mut function) =
                        self.parse_function(false, Execution::Inherited, None)
                    {
                        self.refuse_generic_member(&mut function);
                        body.methods.push(ConstructMethod {
                            computed: false,
                            lifecycle: false,
                            comptime: false,
                            required: false,
                            function,
                        });
                    }
                } else {
                    self.error(
                        self.current().span,
                        "KPAR064",
                        "`@Consuming` annotates a `function` member",
                    );
                    self.consume_deferred_member(body, at_span, "malformed `@Consuming` member");
                }
            }
            other => {
                self.error(
                    name_span,
                    "KPAR061",
                    format!("`@{other}` is not a construct member annotation"),
                );
                self.consume_deferred_member(body, at_span, "unknown annotated member");
            }
        }
    }

    /// Consumes a not-yet-executable member (a `let` field or a `function`) so
    /// the rest of the body still parses, recording it as deferred.
    fn consume_deferred_member(
        &mut self,
        body: &mut ConstructBody,
        start: Span,
        label: &'static str,
    ) {
        match self.current_kind() {
            TokenKind::Let | TokenKind::Var => {
                let mut discard = ConstructBody::default();
                self.parse_construct_let(&mut discard, self.at(TokenKind::Var));
            }
            TokenKind::Function => {
                self.parse_function(false, Execution::Inherited, None);
            }
            _ => {
                // Nothing recognizable follows; step over one token so the loop
                // makes progress.
                self.bump();
            }
        }
        let span = Span::from_bounds(start.start, self.previous_end());
        body.deferred.push(DeferredConstruct { label, span });
    }

    /// Parses `init(params) { body }`, with `init` at the cursor.
    ///
    /// An initializer is a function returning the declaration it is written in,
    /// so it writes no result type: there is only one thing it can produce, and
    /// restating it would be the declaration's name twice on one line. The
    /// result is filled in by analysis, which holds the declaration's id.
    pub(super) fn parse_construct_init(&mut self, body: &mut ConstructBody) {
        let start = self.current().span;
        let name_span = start;
        let name = self.intern_span(name_span);
        self.bump(); // `init`
        let params = self.parse_params();
        if self.at(TokenKind::Arrow) || self.at(TokenKind::Colon) {
            self.error(
                self.current().span,
                "KPAR067",
                "an `init` writes no result type: it produces the declaration it is written in",
            );
            self.parse_return_type();
        }
        let block = self.parse_block();
        let span = Span::from_bounds(start.start, self.previous_end());
        body.inits.push(Function {
            name,
            name_span,
            type_params: Vec::new(),
            is_main: false,
            is_async: false,
            export: None,
            foreign: None,
            execution: Execution::Inherited,
            receiver: None,
            params,
            return_type: None,
            body: block,
            span,
        });
    }

    /// Parses a `requires { … }` section, with `requires` at the cursor.
    ///
    /// Every entry is a bodyless `function` signature, and each becomes exactly
    /// the member `@Required function` would have: the section is a spelling,
    /// not a second kind of requirement. Anything else inside is reported and
    /// skipped so one bad entry costs one diagnostic rather than derailing the
    /// enclosing body.
    pub(super) fn parse_construct_requires_section(&mut self, body: &mut ConstructBody) {
        self.bump(); // `requires`
        self.bump(); // `{`
        while !self.at(TokenKind::RBrace) && !self.at_eof() {
            while self.eat(TokenKind::Semicolon) {}
            if self.at(TokenKind::RBrace) || self.at_eof() {
                break;
            }
            let before = self.pos;
            if self.at(TokenKind::Function) {
                self.parse_construct_required_function(body);
            } else {
                self.error(
                    self.current().span,
                    "KPAR066",
                    format!(
                        "a `requires` section lists `function` signatures, found {}",
                        self.current_kind().describe()
                    ),
                );
                self.recover_to_next_construct_member();
            }
            if self.pos == before {
                self.bump();
            }
        }
        self.expect(TokenKind::RBrace);
    }

    /// Parses `lifecycle { name() { … } … }`, with `lifecycle` at the cursor.
    ///
    /// A hook is an ordinary instance method, which is what lets a runtime
    /// holding a declaration's value call it: an async executor driving
    /// `onStart`, a UI framework driving `onAppear`. The section is how the
    /// family says *which* of its methods are lifecycle points, so a runtime
    /// finds them without knowing any hook by name.
    ///
    /// A hook carrying `@Comptime` runs during compilation instead, and is the
    /// one kind no runtime ever sees.
    pub(super) fn parse_construct_lifecycle_section(&mut self, body: &mut ConstructBody) {
        self.bump(); // `lifecycle`
        self.bump(); // `{`
        while !self.at(TokenKind::RBrace) && !self.at_eof() {
            while self.eat(TokenKind::Semicolon) {}
            if self.at(TokenKind::RBrace) || self.at_eof() {
                break;
            }
            let before = self.pos;
            let comptime = self.eat_comptime_annotation();
            if self.at(TokenKind::Identifier) && self.peek(1).kind == TokenKind::LParen {
                self.parse_construct_hook(body, comptime);
            } else {
                self.error(
                    self.current().span,
                    "KPAR066",
                    format!(
                        "a `lifecycle` section lists `name(…) {{ … }}` hooks, found {}",
                        self.current_kind().describe()
                    ),
                );
                self.recover_to_next_construct_member();
            }
            if self.pos == before {
                self.bump();
            }
        }
        self.expect(TokenKind::RBrace);
    }

    /// Consumes a `@Comptime` annotation, reporting whether one was there.
    fn eat_comptime_annotation(&mut self) -> bool {
        if !self.at(TokenKind::At) || !self.peek_is_word(1, "Comptime") {
            return false;
        }
        self.bump(); // `@`
        self.bump(); // `Comptime`
        true
    }

    /// Parses one `name(params) [-> Type] { … }` hook.
    fn parse_construct_hook(&mut self, body: &mut ConstructBody, comptime: bool) {
        let start = self.current().span;
        let name_span = start;
        let name = self.intern_span(name_span);
        self.bump(); // hook name
        let params = self.parse_params();
        let return_type = self.eat(TokenKind::Arrow).then(|| self.parse_type_ref());
        let block = self.parse_block();
        let span = Span::from_bounds(start.start, self.previous_end());
        body.methods.push(ConstructMethod {
            computed: false,
            lifecycle: true,
            comptime,
            required: false,
            function: Self::hook_function(name, name_span, params, return_type, block, span),
        });
    }

    /// Parses `@Required function name(params) [-> Type]`, with `function` at
    /// the cursor.
    ///
    /// The member is **bodyless**: it states the signature a backed declaration
    /// must implement, so a written `{ … }` is refused rather than stored — a
    /// requirement that carried a body would be an inheritable default, which is
    /// the plain `function` member instead. A trailing `;` is accepted and
    /// consumed, the way the bodyless foreign form ends.
    fn parse_construct_required_function(&mut self, body: &mut ConstructBody) {
        let start = self.current().span;
        self.bump(); // `function`
        let name_span = self.current().span;
        let name = if self.at(TokenKind::Identifier) {
            let symbol = self.intern_span(name_span);
            self.bump();
            symbol
        } else {
            self.error(name_span, "KPAR004", "expected a function name");
            Symbol::ERROR
        };
        self.refuse_type_params("function");
        let (receiver, params) = self.parse_signature_params();
        let return_type = self.parse_return_type();
        let empty = Span::from_bounds(self.previous_end(), self.previous_end());
        if self.at(TokenKind::LBrace) {
            self.error(
                self.current().span,
                "KPAR065",
                "a `@Required function` states a signature and has no body; a member \
                 with a body is an ordinary `function` member",
            );
            self.parse_block();
        }
        self.eat(TokenKind::Semicolon);
        let span = Span::from_bounds(start.start, self.previous_end());
        body.methods.push(ConstructMethod {
            computed: false,
            lifecycle: false,
            comptime: false,
            required: true,
            function: Function {
                name,
                name_span,
                type_params: Vec::new(),
                is_main: false,
                is_async: false,
                export: None,
                foreign: None,
                execution: Execution::Inherited,
                receiver,
                params,
                return_type,
                body: Block {
                    stmts: Vec::new(),
                    span: empty,
                },
                span,
            },
        });
    }

    /// Parses `@Required let name: Any [= default]`, with `let` at the cursor.
    fn parse_construct_let_required(&mut self, body: &mut ConstructBody) {
        let start = self.current().span;
        self.bump(); // `let`
        let Some((name, name_span, ty, slot)) = self.parse_construct_member_head() else {
            return;
        };
        if self.at(TokenKind::LBrace) {
            self.error(
                self.current().span,
                "KPAR062",
                "a `@Required` member has no computed body: it is a value the \
                 backed declaration supplies",
            );
            self.parse_block();
        }
        let default = self.eat(TokenKind::Equals).then(|| self.parse_expr());
        let span = Span::from_bounds(start.start, self.previous_end());
        body.fields.push(ConstructField {
            name,
            name_span,
            required: true,
            mutable: false,
            slot,
            ty,
            default,
            span,
        });
    }

    /// Parses `@Content let name: Any`, with `let` at the cursor — a child slot in
    /// its compat spelling. Equivalent to a `let name: some Any` field.
    fn parse_construct_content_slot(&mut self, body: &mut ConstructBody) {
        let start = self.current().span;
        self.bump(); // `let`
        let Some((name, name_span, ty, _slot)) = self.parse_construct_member_head() else {
            return;
        };
        let default = self.eat(TokenKind::Equals).then(|| self.parse_expr());
        let span = Span::from_bounds(start.start, self.previous_end());
        body.fields.push(ConstructField {
            name,
            name_span,
            required: false,
            mutable: false,
            slot: true,
            ty,
            default,
            span,
        });
    }

    /// Parses a plain or computed `let` construct member, with `let`/`var` at
    /// the cursor.
    pub(super) fn parse_construct_let(&mut self, body: &mut ConstructBody, is_var: bool) {
        let start = self.current().span;
        self.bump(); // `let` / `var`
        let Some((name, name_span, ty, slot)) = self.parse_construct_member_head() else {
            return;
        };
        if self.at(TokenKind::LBrace) {
            let Some(ty) = ty else {
                self.error(
                    self.current().span,
                    "KPAR066",
                    "a computed construct member must declare its result type",
                );
                self.parse_block();
                return;
            };
            // `let node: Any { block }` — a computed member: a zero-argument method
            // read as a property.
            let block = self.parse_block();
            self.make_block_return_its_tail(&block);
            let span = Span::from_bounds(start.start, self.previous_end());
            body.methods.push(ConstructMethod {
                computed: true,
                lifecycle: false,
                comptime: false,
                required: false,
                function: Self::computed_member_function(name, name_span, ty, block, span),
            });
            return;
        }
        let default = self.eat(TokenKind::Equals).then(|| self.parse_expr());
        let span = Span::from_bounds(start.start, self.previous_end());
        body.fields.push(ConstructField {
            name,
            name_span,
            required: false,
            mutable: is_var,
            slot,
            ty,
            default,
            span,
        });
    }

    /// Parses the optional `name: Type` head shared by every `let` construct
    /// member, returning whether the type was written as a child slot (`some X`
    /// / `[some X]`).
    fn parse_construct_member_head(&mut self) -> Option<(Symbol, Span, Option<TypeRefId>, bool)> {
        if !self.at(TokenKind::Identifier) {
            self.error(self.current().span, "KPAR010", "expected a member name");
            return None;
        }
        let name_span = self.current().span;
        let name = self.intern_span(name_span);
        self.bump();
        let (ty, slot) = if self.eat(TokenKind::Colon) {
            let (ty, slot) = self.parse_construct_field_type();
            (Some(ty), slot)
        } else {
            (None, false)
        };
        Some((name, name_span, ty, slot))
    }

    /// Parses a construct field's type, reporting whether it was written as a
    /// child slot (`some X` / `[some X]`).
    ///
    /// The type itself is the ordinary one [`Parser::parse_type_ref`] gives —
    /// `some X` is a type everywhere, not a spelling this position invents. What
    /// is local to a construct field is only what the slot spelling *means*
    /// here: a caller-provided child rather than a constructor argument. A
    /// single slot and a list slot are told apart downstream by whether the type
    /// is an array.
    fn parse_construct_field_type(&mut self) -> (TypeRefId, bool) {
        let slot = self.at_some_construct() || self.at_bracketed_some_construct();
        (self.parse_type_ref(), slot)
    }

    /// Rewrites a block's trailing expression statement into a `return`, in
    /// place.
    ///
    /// A computed member is an expression bridge — `let node: Node { body.node }`
    /// yields its final expression — so its block returns its tail, the way the
    /// source reads it. Statements live in the tree's arena, so the rewrite
    /// replaces the arena entry the block's last id names.
    pub(super) fn make_block_return_its_tail(&mut self, body: &Block) {
        let Some(&last) = body.stmts.last() else {
            return;
        };
        match self.tree.stmt(last) {
            Stmt::Expr { expr, span } => {
                let (expr, span) = (*expr, *span);
                *self.tree.stmt_mut(last) = Stmt::Return {
                    value: Some(expr),
                    span,
                };
            }
            // A member whose value is chosen by a condition ends in the `if`
            // rather than in an expression, and each arm is then a tail of its
            // own. Recursing gives every arm the return the single-expression
            // case gets — including a nested `else if`, which is an `if` in the
            // else block. An `if` with no `else` is left alone: one arm returning
            // does not make the member total, and the definite-return check is
            // what should say so.
            Stmt::If {
                then_block,
                else_block: Some(else_block),
                ..
            } => {
                let (then_block, else_block) = (then_block.clone(), else_block.clone());
                self.make_block_return_its_tail(&then_block);
                self.make_block_return_its_tail(&else_block);
            }
            _ => {}
        }
    }

    /// Builds the zero-argument method a computed member (`let node: Any { … }`)
    /// desugars to: no parameters, result type `Any`, body the written block.
    /// The `Function` one lifecycle hook lowers to.
    ///
    /// An ordinary method: the runtime that drives it calls it like any other,
    /// and the family's section is the only thing that says it is a hook.
    fn hook_function(
        name: Symbol,
        name_span: Span,
        params: Vec<Param>,
        return_type: Option<TypeRefId>,
        body: Block,
        span: Span,
    ) -> Function {
        Function {
            name,
            name_span,
            type_params: Vec::new(),
            is_main: false,
            is_async: false,
            foreign: None,
            export: None,
            execution: Execution::Inherited,
            receiver: None,
            params,
            return_type,
            body,
            span,
        }
    }

    pub(super) fn computed_member_function(
        name: Symbol,
        name_span: Span,
        ty: TypeRefId,
        body: Block,
        span: Span,
    ) -> Function {
        Function {
            name,
            name_span,
            type_params: Vec::new(),
            is_main: false,
            is_async: false,
            foreign: None,
            export: None,
            execution: Execution::Inherited,
            receiver: None,
            params: Vec::new(),
            return_type: Some(ty),
            body,
            span,
        }
    }
}
