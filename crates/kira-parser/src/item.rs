//! Top-level item parsing: functions, structs, and the parse-don't-crash path
//! for constructs the v0 subset does not analyze yet.
//!
//! Recovery boundary: an item that cannot be parsed consumes its balanced
//! `{...}` body if it has one and becomes an [`Item::Unsupported`] node, so one
//! malformed declaration never derails the rest of the file.

use kira_core::Symbol;
use kira_diagnostics::{Diagnostic, Label, Severity};
use kira_runtime_abi::Execution;
use kira_source::{FileSpan, Span};
use kira_syntax_model::TokenKind;
use kira_syntax_model::ast::{
    Block, Function, ImportDecl, Item, Param, TypeAliasDecl, TypeRef, TypeRefId, UnsupportedItem,
};
use kira_syntax_model::ownership::OwnershipMode;

use crate::Parser;

impl Parser<'_> {
    pub(crate) fn parse_item(&mut self) {
        match self.current_kind() {
            TokenKind::At => self.parse_annotated_item(),
            TokenKind::Function => {
                if let Some(function) = self.parse_function(false, Execution::Inherited) {
                    self.tree.push_item(self.source, Item::Function(function));
                }
            }
            TokenKind::Struct => {
                if let Some(declaration) = self.parse_struct() {
                    self.tree.push_item(self.source, Item::Struct(declaration));
                }
            }
            TokenKind::Enum => {
                if let Some(declaration) = self.parse_enum() {
                    self.tree.push_item(self.source, Item::Enum(declaration));
                }
            }
            TokenKind::Type => {
                if let Some(declaration) = self.parse_type_alias() {
                    self.tree
                        .push_item(self.source, Item::TypeAlias(declaration));
                }
            }
            TokenKind::Import => {
                if let Some(declaration) = self.parse_import() {
                    self.tree.push_item(self.source, Item::Import(declaration));
                }
            }
            TokenKind::Class => {
                if let Some(declaration) = self.parse_class() {
                    self.tree.push_item(self.source, Item::Class(declaration));
                }
            }
            TokenKind::Identifier => self.parse_unsupported_item(),
            _ => {
                // Stray token at top level: skip it with a diagnostic.
                let span = self.current().span;
                self.error(
                    span,
                    "KPAR002",
                    format!("unexpected {} at top level", self.current_kind().describe()),
                );
                self.bump();
            }
        }
    }

    fn parse_annotated_item(&mut self) {
        let start = self.current().span;
        let mut is_main = false;
        let mut execution = Execution::Inherited;
        // Consume one or more `@Name` annotations.
        while self.at(TokenKind::At) {
            self.bump();
            if self.at(TokenKind::Identifier) {
                let name_span = self.current().span;
                match self.text_of(name_span) {
                    "Main" => is_main = true,
                    name => {
                        if let Some(selected) = Execution::from_annotation(name) {
                            // Two engines on one function is a contradiction,
                            // not a refinement: the second would silently win.
                            if execution != Execution::Inherited && execution != selected {
                                self.error(
                                    name_span,
                                    "KPAR005",
                                    "a function selects one execution engine; \
                                     `@Runtime` and `@Native` cannot both apply",
                                );
                            }
                            execution = selected;
                        }
                    }
                }
                self.bump();
                // Skip an optional `(...)` annotation argument list.
                if self.at(TokenKind::LParen) {
                    self.skip_balanced(TokenKind::LParen, TokenKind::RParen);
                }
            } else {
                self.error(
                    self.current().span,
                    "KPAR003",
                    "expected an annotation name after `@`",
                );
                break;
            }
        }
        if self.at(TokenKind::Function) {
            if let Some(function) = self.parse_function(is_main, execution) {
                self.tree.push_item(self.source, Item::Function(function));
            }
        } else {
            // Annotated non-function construct: parse-don't-crash.
            self.parse_unsupported_item_from(start);
        }
    }

    pub(crate) fn parse_function(
        &mut self,
        is_main: bool,
        execution: Execution,
    ) -> Option<Function> {
        let start = self.current().span;
        self.expect(TokenKind::Function);
        let (name, name_span) = if self.at(TokenKind::Identifier) {
            let span = self.current().span;
            (self.intern_span(span), span)
        } else {
            self.error(self.current().span, "KPAR004", "expected a function name");
            (Symbol::ERROR, self.current().span)
        };
        if self.at(TokenKind::Identifier) {
            self.bump();
        }
        let params = self.parse_params();
        let return_type = self.parse_return_type();
        let body = self.parse_block();
        let span = Span::from_bounds(start.start, self.previous_end());
        Some(Function {
            name,
            name_span,
            is_main,
            execution,
            params,
            return_type,
            body,
            span,
        })
    }

    // ----- type aliases --------------------------------------------------

    /// Parses `type Name = Target`.
    ///
    /// The target is an ordinary type reference, so `type ByteMatrix =
    /// [[Byte]]` needs no grammar of its own. A missing name yields no node at
    /// all: an alias with nothing to bind would register a name nobody wrote,
    /// and the `type` keyword is already consumed, so recovery still advances.
    fn parse_type_alias(&mut self) -> Option<TypeAliasDecl> {
        let start = self.current().span;
        self.expect(TokenKind::Type);
        if !self.at(TokenKind::Identifier) {
            self.error(self.current().span, "KPAR032", "expected a type alias name");
            return None;
        }
        let name_span = self.current().span;
        let name = self.intern_span(name_span);
        self.bump();
        // `=` is required, not optional: `type Name` on its own aliases nothing.
        self.expect(TokenKind::Equals);
        let target = self.parse_type_ref();
        let span = Span::from_bounds(start.start, self.previous_end());
        Some(TypeAliasDecl {
            name,
            name_span,
            target,
            span,
        })
    }

    // ----- shared signature pieces ---------------------------------------

    fn parse_params(&mut self) -> Vec<Param> {
        let mut params = Vec::new();
        if !self.expect(TokenKind::LParen) {
            return params;
        }
        while !self.at(TokenKind::RParen) && !self.at_eof() {
            let before = self.pos;
            if let Some(param) = self.parse_param() {
                params.push(param);
            }
            if !self.eat(TokenKind::Comma) {
                break;
            }
            if self.pos == before {
                self.bump();
            }
        }
        self.expect(TokenKind::RParen);
        params
    }

    fn parse_param(&mut self) -> Option<Param> {
        if !self.at(TokenKind::Identifier) {
            self.error(self.current().span, "KPAR005", "expected a parameter name");
            return None;
        }
        let name_span = self.current().span;
        let name = self.intern_span(name_span);
        self.bump();
        self.expect(TokenKind::Colon);
        let (ownership, ownership_span) = self.parse_param_ownership();
        let ty = self.parse_type_ref();
        let span = Span::from_bounds(name_span.start, self.previous_end());
        Some(Param {
            name,
            name_span,
            ownership,
            ownership_span,
            ty,
            span,
        })
    }

    /// Parses the ownership prefix of a parameter type, if one is written.
    ///
    /// Accepts `borrow`, `borrow mut`, `move`, and `copy`. All four are
    /// contextual identifiers, so each is committed to only when a type name
    /// follows it — that is what keeps `f(borrow: Int)` (a parameter *named*
    /// `borrow`) and `f(x: move)` (a type named `move`, were one declared)
    /// parsing as they always did. A bare type yields
    /// [`OwnershipMode::Owned`], which is the default rather than a fallback.
    fn parse_param_ownership(&mut self) -> (OwnershipMode, Option<Span>) {
        if self.at_word("borrow") && self.peek_is_word(1, "mut") && self.peek_starts_type(2) {
            let start = self.current().span;
            self.bump(); // `borrow`
            let end = self.current().span;
            self.bump(); // `mut`
            return (
                OwnershipMode::BorrowMut,
                Some(Span::from_bounds(start.start, end.end())),
            );
        }
        if self.at_word("borrow") && self.peek_starts_type(1) {
            let span = self.current().span;
            self.bump();
            return (OwnershipMode::BorrowRead, Some(span));
        }
        for (word, mode) in [("move", OwnershipMode::Move), ("copy", OwnershipMode::Copy)] {
            if self.at_word(word) && self.peek_starts_type(1) {
                let span = self.current().span;
                self.bump();
                return (mode, Some(span));
            }
        }
        (OwnershipMode::Owned, None)
    }

    fn parse_return_type(&mut self) -> Option<TypeRefId> {
        // Kira accepts both `-> Type` and `): Type`.
        if self.eat(TokenKind::Arrow) || self.eat(TokenKind::Colon) {
            Some(self.parse_type_ref())
        } else {
            None
        }
    }

    /// Whether the token `n` ahead can begin a written type.
    ///
    /// A type starts with a name (`Int`, `Point`), with `[` (`[Int]`), or with
    /// `(` (`(Int) -> Void`). This is what every contextual-keyword lookahead
    /// asks, and asking it in one place is what keeps `borrow [Int]` from
    /// silently parsing as a parameter whose type is named `borrow`.
    pub(crate) fn peek_starts_type(&self, n: usize) -> bool {
        matches!(
            self.peek(n).kind,
            TokenKind::Identifier | TokenKind::LBracket | TokenKind::LParen
        )
    }

    /// Parses `import Module[.Sub…] [as Alias]`.
    ///
    /// Recovery: a malformed path yields no item at all rather than a partial
    /// one, because an import with no module names nothing a later phase could
    /// resolve — the parser has already said what was wrong, and inventing a
    /// module would produce a second, misleading "unresolved import".
    fn parse_import(&mut self) -> Option<ImportDecl> {
        let start = self.current().span;
        self.expect(TokenKind::Import);
        let mut path = Vec::new();
        let path_start = self.current().span;
        loop {
            if !self.at(TokenKind::Identifier) {
                self.error(
                    self.current().span,
                    "KPAR016",
                    "expected a module name after `import`",
                );
                return None;
            }
            let span = self.current().span;
            path.push(self.intern_span(span));
            self.bump();
            if !self.eat(TokenKind::Dot) {
                break;
            }
        }
        let path_span = Span::from_bounds(path_start.start, self.previous_end());
        // `as` is a keyword, so the alias clause needs no contextual lookahead.
        let (alias, alias_span) = if self.eat(TokenKind::As) {
            if self.at(TokenKind::Identifier) {
                let span = self.current().span;
                let symbol = self.intern_span(span);
                self.bump();
                (Some(symbol), Some(span))
            } else {
                self.error(self.current().span, "KPAR017", "expected a name after `as`");
                (None, None)
            }
        } else {
            (None, None)
        };
        let span = Span::from_bounds(start.start, self.previous_end());
        Some(ImportDecl {
            path,
            path_span,
            alias,
            alias_span,
            span,
        })
    }

    /// Parses a written type: a name, `[` element `]`, or a function type,
    /// nested to any depth.
    ///
    /// A name may be **module-qualified** (`Support.Point`). The qualifier is
    /// kept in the interned name — a dot cannot appear in an identifier, so a
    /// qualified spelling can never collide with a declared one — and semantics
    /// is what strips it against the file's imports.
    ///
    /// A leading `(` always starts a function type: no other written type is
    /// parenthesized, so there is nothing to disambiguate against. That is also
    /// why a function result type is spelled with `:` rather than `->` on a
    /// declaration — `function f(): (Int) -> Int` — and both spellings are
    /// accepted for every other result type.
    pub(crate) fn parse_type_ref(&mut self) -> TypeRefId {
        if self.at(TokenKind::LParen) {
            return self.parse_function_type();
        }
        if self.at(TokenKind::LBracket) {
            let start = self.current().span;
            self.bump(); // `[`
            let element = self.parse_type_ref();
            self.expect(TokenKind::RBracket);
            let span = Span::from_bounds(start.start, self.previous_end());
            return self.tree.add_type(TypeRef::Array { element, span });
        }
        if self.at(TokenKind::Identifier) {
            let start = self.current().span;
            let mut text = self.text_of(start).to_owned();
            self.bump();
            while self.at(TokenKind::Dot) && self.peek(1).kind == TokenKind::Identifier {
                self.bump(); // `.`
                let segment = self.current().span;
                text.push('.');
                text.push_str(self.text_of(segment));
                self.bump();
            }
            let span = Span::from_bounds(start.start, self.previous_end());
            let name = self.intern_text(&text, span);
            return self.tree.add_type(TypeRef::Named { name, span });
        }
        let span = self.current().span;
        self.error(span, "KPAR006", "expected a type name");
        self.tree.add_type(TypeRef::Error { span })
    }

    /// Parses `(A, B) -> R`, with the cursor on `(`.
    ///
    /// The result is mandatory: a function type with no `->` names nothing, so
    /// a missing arrow is reported and the whole type recovers to
    /// [`TypeRef::Error`] rather than silently becoming `() -> Void`.
    fn parse_function_type(&mut self) -> TypeRefId {
        let start = self.current().span;
        self.bump(); // `(`
        let mut params = Vec::new();
        while !self.at(TokenKind::RParen) && !self.at_eof() {
            let before = self.pos;
            params.push(self.parse_type_ref());
            self.eat(TokenKind::Comma);
            // A parameter that consumed nothing would spin; force progress.
            if self.pos == before {
                self.bump();
            }
        }
        self.expect(TokenKind::RParen);
        if !self.eat(TokenKind::Arrow) {
            let span = Span::from_bounds(start.start, self.previous_end());
            self.error(span, "KPAR038", "expected `->` in a function type");
            return self.tree.add_type(TypeRef::Error { span });
        }
        let result = self.parse_type_ref();
        let span = Span::from_bounds(start.start, self.previous_end());
        self.tree.add_type(TypeRef::Function {
            params,
            result,
            span,
        })
    }

    pub(crate) fn parse_block(&mut self) -> Block {
        let start = self.current().span;
        if !self.expect(TokenKind::LBrace) {
            return Block {
                stmts: Vec::new(),
                span: start,
            };
        }
        self.parse_block_body(start)
    }

    /// Parses statements up to and including the closing `}`, with the opening
    /// `{` (whose span is `start`) already consumed.
    ///
    /// Split out because a closure's body is the same statement list behind the
    /// same brace, only reached after its parameters and `in` were consumed —
    /// so it cannot call [`Parser::parse_block`], which would demand a second
    /// `{`.
    pub(crate) fn parse_block_body(&mut self, start: Span) -> Block {
        let mut stmts = Vec::new();
        while !self.at(TokenKind::RBrace) && !self.at_eof() {
            let before = self.pos;
            while self.eat(TokenKind::Semicolon) {}
            if self.at(TokenKind::RBrace) || self.at_eof() {
                break;
            }
            if let Some(stmt) = self.parse_stmt() {
                stmts.push(stmt);
            }
            while self.eat(TokenKind::Semicolon) {}
            if self.pos == before {
                self.bump();
            }
        }
        self.expect(TokenKind::RBrace);
        let span = Span::from_bounds(start.start, self.previous_end());
        Block { stmts, span }
    }

    // ----- unsupported constructs (parse-don't-crash) -------------------

    fn parse_unsupported_item(&mut self) {
        let start = self.current().span;
        self.parse_unsupported_item_from(start);
    }

    fn parse_unsupported_item_from(&mut self, start: Span) {
        let keyword = unsupported_keyword(self.current_kind(), self.text_of(self.current().span));
        // Walk forward: if a `{...}` body appears before the next top-level
        // starter, consume it balanced; otherwise stop at the next starter.
        while !self.at_eof() {
            match self.current_kind() {
                TokenKind::LBrace => {
                    self.skip_balanced(TokenKind::LBrace, TokenKind::RBrace);
                    break;
                }
                kind if is_item_start(kind) && self.current().span != start => break,
                _ => {
                    self.bump();
                }
            }
        }
        let span = Span::from_bounds(start.start, self.previous_end());
        self.tree.push_item(
            self.source,
            Item::Unsupported(UnsupportedItem { keyword, span }),
        );
        let file_span = FileSpan::new(self.source, span);
        let mut diagnostic = Diagnostic::single(
            Severity::Error,
            format!("`{keyword}` is not supported yet"),
            Label::primary(file_span, "not yet supported in this compiler"),
        );
        diagnostic.code = Some("KSEM900");
        diagnostic.phase = Some("parser");
        diagnostic.help = Some(
            "the v0 subset supports functions, structs, let/var, if/while, and arithmetic"
                .to_owned(),
        );
        self.diagnostics.push(diagnostic);
    }
}

/// Whether `kind` can begin a top-level item, used to bound error recovery.
fn is_item_start(kind: TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::At
            | TokenKind::Function
            | TokenKind::Struct
            | TokenKind::Enum
            | TokenKind::Type
            | TokenKind::Class
            | TokenKind::Import
    )
}

/// A stable label for an unsupported construct, for diagnostics.
fn unsupported_keyword(kind: TokenKind, text: &str) -> &'static str {
    match kind {
        TokenKind::Enum => "enum",
        TokenKind::Class => "class",
        TokenKind::Import => "import",
        TokenKind::Identifier => match text {
            "Package" => "Package",
            "Test" => "Test",
            "construct" => "construct",
            _ => "declaration",
        },
        _ => "declaration",
    }
}
