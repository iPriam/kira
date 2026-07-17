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
    Block, FieldDecl, Function, Item, Param, StructDecl, TypeRef, UnsupportedItem,
};

use crate::Parser;

impl Parser<'_> {
    pub(crate) fn parse_item(&mut self) {
        match self.current_kind() {
            TokenKind::At => self.parse_annotated_item(),
            TokenKind::Function => {
                if let Some(function) = self.parse_function(false, Execution::Inherited) {
                    self.tree.items.push(Item::Function(function));
                }
            }
            TokenKind::Struct => {
                if let Some(declaration) = self.parse_struct() {
                    self.tree.items.push(Item::Struct(declaration));
                }
            }
            TokenKind::Enum | TokenKind::Class | TokenKind::Import | TokenKind::Identifier => {
                self.parse_unsupported_item()
            }
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
                self.tree.items.push(Item::Function(function));
            }
        } else {
            // Annotated non-function construct: parse-don't-crash.
            self.parse_unsupported_item_from(start);
        }
    }

    fn parse_function(&mut self, is_main: bool, execution: Execution) -> Option<Function> {
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

    // ----- structs ------------------------------------------------------

    /// Parses `struct Name { <member>* }`.
    ///
    /// Members are `let`/`var` bindings. Newlines and `;` are both
    /// insignificant, so the member keyword is what starts each member and a
    /// member with no keyword is reported rather than silently skipped.
    fn parse_struct(&mut self) -> Option<StructDecl> {
        let start = self.current().span;
        self.expect(TokenKind::Struct);
        let (name, name_span) = if self.at(TokenKind::Identifier) {
            let span = self.current().span;
            (self.intern_span(span), span)
        } else {
            self.error(self.current().span, "KPAR007", "expected a struct name");
            (Symbol::ERROR, self.current().span)
        };
        if self.at(TokenKind::Identifier) {
            self.bump();
        }
        let mut fields = Vec::new();
        if !self.expect(TokenKind::LBrace) {
            let span = Span::from_bounds(start.start, self.previous_end());
            return Some(StructDecl {
                name,
                name_span,
                fields,
                span,
            });
        }
        while !self.at(TokenKind::RBrace) && !self.at_eof() {
            let before = self.pos;
            while self.eat(TokenKind::Semicolon) {}
            if self.at(TokenKind::RBrace) || self.at_eof() {
                break;
            }
            match self.current_kind() {
                TokenKind::Let => {
                    if let Some(field) = self.parse_field(false) {
                        fields.push(field);
                    }
                }
                TokenKind::Var => {
                    if let Some(field) = self.parse_field(true) {
                        fields.push(field);
                    }
                }
                TokenKind::Function => {
                    // Methods are language surface this compiler does not model
                    // yet. Skip the whole member so the struct's fields still
                    // parse, and say so once, on the method itself.
                    let span = self.current().span;
                    self.error(
                        span,
                        "KPAR008",
                        "struct methods are not supported yet; this struct's stored \
                         members still parse",
                    );
                    self.bump();
                    while !self.at_eof()
                        && !self.at(TokenKind::LBrace)
                        && !self.at(TokenKind::RBrace)
                    {
                        self.bump();
                    }
                    if self.at(TokenKind::LBrace) {
                        self.skip_balanced(TokenKind::LBrace, TokenKind::RBrace);
                    }
                }
                kind => {
                    let span = self.current().span;
                    self.error(
                        span,
                        "KPAR009",
                        format!(
                            "expected `let` or `var` to start a struct member, found {}",
                            kind.describe()
                        ),
                    );
                    self.bump();
                }
            }
            while self.eat(TokenKind::Semicolon) {}
            if self.pos == before {
                self.bump();
            }
        }
        self.expect(TokenKind::RBrace);
        let span = Span::from_bounds(start.start, self.previous_end());
        Some(StructDecl {
            name,
            name_span,
            fields,
            span,
        })
    }

    /// Parses one `let`/`var` struct member, with the keyword at the cursor.
    fn parse_field(&mut self, mutable: bool) -> Option<FieldDecl> {
        let start = self.current().span;
        self.bump(); // `let` / `var`
        if !self.at(TokenKind::Identifier) {
            self.error(self.current().span, "KPAR010", "expected a member name");
            return None;
        }
        let name_span = self.current().span;
        let name = self.intern_span(name_span);
        self.bump();
        // A member's type is required: a struct's shape is its contract, and
        // there is no initializer to infer it from when the default is absent.
        self.expect(TokenKind::Colon);
        let ty = self.parse_type_ref();
        let default = self.eat(TokenKind::Equals).then(|| self.parse_expr());
        let span = Span::from_bounds(start.start, self.previous_end());
        Some(FieldDecl {
            name,
            name_span,
            mutable,
            ty,
            default,
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
        let ty = self.parse_type_ref();
        let span = Span::from_bounds(name_span.start, self.previous_end());
        Some(Param {
            name,
            name_span,
            ty,
            span,
        })
    }

    fn parse_return_type(&mut self) -> Option<TypeRef> {
        // Kira accepts both `-> Type` and `): Type`.
        if self.eat(TokenKind::Arrow) || self.eat(TokenKind::Colon) {
            Some(self.parse_type_ref())
        } else {
            None
        }
    }

    pub(crate) fn parse_type_ref(&mut self) -> TypeRef {
        if self.at(TokenKind::Identifier) {
            let span = self.current().span;
            let name = self.intern_span(span);
            self.bump();
            TypeRef { name, span }
        } else {
            let span = self.current().span;
            self.error(span, "KPAR006", "expected a type name");
            TypeRef {
                name: Symbol::ERROR,
                span,
            }
        }
    }

    pub(crate) fn parse_block(&mut self) -> Block {
        let start = self.current().span;
        let mut stmts = Vec::new();
        if !self.expect(TokenKind::LBrace) {
            return Block { stmts, span: start };
        }
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
        self.tree
            .items
            .push(Item::Unsupported(UnsupportedItem { keyword, span }));
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
