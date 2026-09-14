use kira_source::Span;
use kira_syntax_model::TokenKind;
use kira_syntax_model::ast::{ImportDecl, Item};

use crate::Parser;

impl Parser<'_> {
    /// `namespace A.B.C { let X = … }` — a nested named scope.
    ///
    /// Kira has no runtime notion of a namespace: a namespace is a *name*. Each
    /// member is flattened here into an ordinary module constant whose name is
    /// the member qualified by the dotted path —
    /// `AI.Providers.OpenAI.GPT.5.6.Sol` — and an access spelled the same way
    /// resolves to it in semantics. A dot cannot appear in an identifier, so a
    /// qualified name never collides with a declared one, exactly as a
    /// module-qualified type reference does. `let` is static already, so a
    /// member is a `let`; a nested `namespace` extends the path.
    pub(super) fn parse_namespace(&mut self) {
        self.expect(TokenKind::Namespace);
        let Some(prefix) = self.parse_namespace_path() else {
            return;
        };
        self.parse_namespace_body(&prefix);
    }

    /// The dotted path after `namespace`, as one string.
    ///
    /// A segment is an identifier or a numeric token: a version-like `5.6` is
    /// one float token the lexer already read, and its written text is the
    /// segment, so the dotted spelling an access reconstructs matches exactly.
    fn parse_namespace_path(&mut self) -> Option<String> {
        let mut path = String::new();
        loop {
            if self.at(TokenKind::Identifier)
                || self.at(TokenKind::IntLiteral)
                || self.at(TokenKind::FloatLiteral)
            {
                let segment = self.text_of(self.current().span).to_owned();
                if !path.is_empty() {
                    path.push('.');
                }
                path.push_str(&segment);
                self.bump();
            } else {
                self.error(
                    self.current().span,
                    "KPAR088",
                    "expected a namespace path segment",
                );
                return None;
            }
            if !self.eat(TokenKind::Dot) {
                break;
            }
        }
        Some(path)
    }

    /// The `{ … }` body of a namespace at dotted `prefix`, flattening each member
    /// into a top-level declaration qualified by the prefix.
    fn parse_namespace_body(&mut self, prefix: &str) {
        if !self.expect(TokenKind::LBrace) {
            return;
        }
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            match self.current_kind() {
                TokenKind::Let => {
                    if let Some(mut constant) = self.parse_constant() {
                        let qualified = format!("{prefix}.{}", self.text_of(constant.name_span));
                        constant.name = self.intern_text(&qualified, constant.name_span);
                        self.items.push(Item::Constant(constant));
                    }
                }
                TokenKind::Namespace => {
                    self.bump();
                    if let Some(nested) = self.parse_namespace_path() {
                        let joined = format!("{prefix}.{nested}");
                        self.parse_namespace_body(&joined);
                    }
                }
                _ => {
                    self.error(
                        self.current().span,
                        "KPAR089",
                        "a namespace holds `let` members and nested namespaces",
                    );
                    self.bump();
                }
            }
        }
        self.expect(TokenKind::RBrace);
    }

    /// Parses `import Module[.Sub…] [as Alias]`.
    pub(super) fn parse_import(&mut self) -> Option<ImportDecl> {
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
}
