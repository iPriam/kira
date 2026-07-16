//! Parser producing the Kira syntax tree from tokens.
//!
//! Layer 1 of the Kira package graph.
//!
//! The parser is hand-written recursive descent and error-resilient: it always
//! produces a [`SyntaxTree`] plus diagnostics and never bails on the first
//! error. Recovery happens at statement and item boundaries, so one malformed
//! construct never derails the rest of the file — the language server and the
//! compiler share this one frontend. The parser owns no global state; it
//! interns identifiers into an [`Interner`] returned alongside the tree.

mod expr;
mod stmt;

use kira_core::{Interner, Symbol};
use kira_diagnostics::{Diagnostic, Label, Severity};
use kira_source::{FileSpan, SourceId, Span};
use kira_syntax_model::ast::{Block, Function, Item, Param, TypeRef, UnsupportedItem};
use kira_syntax_model::{SyntaxTree, Token, TokenKind};

/// The result of parsing one source file.
#[derive(Debug, Clone)]
pub struct ParseResult {
    /// The parsed syntax tree (always produced, possibly with error nodes).
    pub tree: SyntaxTree,
    /// The interner holding every identifier symbol referenced by the tree.
    pub interner: Interner,
    /// Diagnostics produced while parsing.
    pub diagnostics: Vec<Diagnostic>,
}

/// Lexes and parses `text`, attributing spans to `source`.
///
/// This is the single entry point the frontend calls; it runs the lexer and
/// then the parser, merging their diagnostics in source order.
pub fn parse(source: SourceId, text: &str) -> ParseResult {
    let lexed = kira_lexer::lex(source, text);
    let mut parser = Parser::new(source, text, lexed.tokens);
    parser.diagnostics = lexed.diagnostics;
    parser.parse_program()
}

/// The parser's mutable working state.
struct Parser<'a> {
    source: SourceId,
    text: &'a str,
    tokens: Vec<Token>,
    pos: usize,
    tree: SyntaxTree,
    interner: Interner,
    diagnostics: Vec<Diagnostic>,
}

impl<'a> Parser<'a> {
    fn new(source: SourceId, text: &'a str, tokens: Vec<Token>) -> Self {
        Self {
            source,
            text,
            tokens,
            pos: 0,
            tree: SyntaxTree::new(),
            interner: Interner::new(),
            diagnostics: Vec::new(),
        }
    }

    fn parse_program(mut self) -> ParseResult {
        while !self.at_eof() {
            let before = self.pos;
            self.parse_item();
            // Safety net: recovery must always make progress.
            if self.pos == before {
                self.pos += 1;
            }
        }
        ParseResult {
            tree: self.tree,
            interner: self.interner,
            diagnostics: self.diagnostics,
        }
    }

    // ----- token cursor -------------------------------------------------

    fn current(&self) -> Token {
        self.tokens[self.pos.min(self.tokens.len() - 1)]
    }

    fn current_kind(&self) -> TokenKind {
        self.current().kind
    }

    fn peek_kind(&self, offset: usize) -> TokenKind {
        let index = (self.pos + offset).min(self.tokens.len() - 1);
        self.tokens[index].kind
    }

    fn at(&self, kind: TokenKind) -> bool {
        self.current_kind() == kind
    }

    fn at_eof(&self) -> bool {
        self.at(TokenKind::Eof)
    }

    fn bump(&mut self) -> Token {
        let token = self.current();
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
        token
    }

    /// Consumes the current token when it matches `kind`; returns whether it did.
    fn eat(&mut self, kind: TokenKind) -> bool {
        if self.at(kind) {
            self.bump();
            true
        } else {
            false
        }
    }

    /// Consumes `kind`, or reports "expected …, found …" and consumes nothing.
    fn expect(&mut self, kind: TokenKind) -> bool {
        if self.eat(kind) {
            return true;
        }
        let found = self.current_kind();
        let span = self.current().span;
        self.error(
            span,
            "KPAR001",
            format!("expected {}, found {}", kind.describe(), found.describe()),
        );
        false
    }

    fn text_of(&self, span: Span) -> &str {
        span.slice(self.text)
    }

    fn intern_span(&mut self, span: Span) -> Symbol {
        let text = span.slice(self.text).to_owned();
        self.interner.intern(&text)
    }

    fn error(&mut self, span: Span, code: &'static str, message: impl Into<String>) {
        let message = message.into();
        let file_span = FileSpan::new(self.source, span);
        let mut diagnostic = Diagnostic::single(
            Severity::Error,
            message.clone(),
            Label::primary(file_span, message),
        );
        diagnostic.code = Some(code);
        diagnostic.phase = Some("parser");
        self.diagnostics.push(diagnostic);
    }

    // ----- items --------------------------------------------------------

    fn parse_item(&mut self) {
        match self.current_kind() {
            TokenKind::At => self.parse_annotated_item(),
            TokenKind::Function => {
                if let Some(function) = self.parse_function(false) {
                    self.tree.items.push(Item::Function(function));
                }
            }
            TokenKind::Struct
            | TokenKind::Enum
            | TokenKind::Class
            | TokenKind::Import
            | TokenKind::Identifier => self.parse_unsupported_item(),
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
        // Consume one or more `@Name` annotations.
        while self.at(TokenKind::At) {
            self.bump();
            if self.at(TokenKind::Identifier) {
                let name_span = self.current().span;
                if self.text_of(name_span) == "Main" {
                    is_main = true;
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
            if let Some(function) = self.parse_function(is_main) {
                self.tree.items.push(Item::Function(function));
            }
        } else {
            // Annotated non-function construct: parse-don't-crash.
            self.parse_unsupported_item_from(start);
        }
    }

    fn parse_function(&mut self, is_main: bool) -> Option<Function> {
        let start = self.current().span;
        self.expect(TokenKind::Function);
        let (name, name_span) = if self.at(TokenKind::Identifier) {
            let span = self.current().span;
            (self.intern_span(span), span)
        } else {
            self.error(self.current().span, "KPAR004", "expected a function name");
            (self.interner.intern("<error>"), self.current().span)
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
            params,
            return_type,
            body,
            span,
        })
    }

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

    fn parse_type_ref(&mut self) -> TypeRef {
        if self.at(TokenKind::Identifier) {
            let span = self.current().span;
            let name = self.intern_span(span);
            self.bump();
            TypeRef { name, span }
        } else {
            let span = self.current().span;
            self.error(span, "KPAR006", "expected a type name");
            TypeRef {
                name: self.interner.intern("<error>"),
                span,
            }
        }
    }

    fn parse_block(&mut self) -> Block {
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
        diagnostic.help =
            Some("the v0 subset supports functions, let/var, if/while, and arithmetic".to_owned());
        self.diagnostics.push(diagnostic);
    }

    /// Consumes a balanced `open`..`close` group, assuming the cursor sits on
    /// `open`. Nested groups of the same delimiter are tracked by depth.
    fn skip_balanced(&mut self, open: TokenKind, close: TokenKind) {
        if !self.at(open) {
            return;
        }
        let mut depth = 0;
        while !self.at_eof() {
            let kind = self.current_kind();
            if kind == open {
                depth += 1;
            } else if kind == close {
                depth -= 1;
                if depth == 0 {
                    self.bump();
                    break;
                }
            }
            self.bump();
        }
    }

    fn previous_end(&self) -> u32 {
        if self.pos == 0 {
            0
        } else {
            self.tokens[self.pos - 1].span.end()
        }
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
        TokenKind::Struct => "struct",
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

#[cfg(test)]
mod tests {
    use super::*;
    use kira_syntax_model::ast::Item;

    fn parse_text(text: &str) -> ParseResult {
        parse(SourceId::new(0), text)
    }

    #[test]
    fn parses_a_main_function() {
        let result = parse_text("@Main\nfunction main() { print(1) return }");
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        assert_eq!(result.tree.items.len(), 1);
        match &result.tree.items[0] {
            Item::Function(f) => {
                assert!(f.is_main);
                assert_eq!(result.interner.resolve(f.name), "main");
            }
            other => panic!("expected function, got {other:?}"),
        }
    }

    #[test]
    fn parses_params_and_return_type() {
        let result = parse_text("function add(a: Int, b: Int) -> Int { return a + b }");
        match &result.tree.items[0] {
            Item::Function(f) => {
                assert_eq!(f.params.len(), 2);
                assert!(f.return_type.is_some());
            }
            other => panic!("expected function, got {other:?}"),
        }
    }

    #[test]
    fn colon_return_type_is_accepted() {
        let result = parse_text("function f(): Int { return 1 }");
        match &result.tree.items[0] {
            Item::Function(f) => assert!(f.return_type.is_some()),
            other => panic!("expected function, got {other:?}"),
        }
    }

    #[test]
    fn unsupported_constructs_do_not_crash() {
        let result = parse_text("struct P { let x: Int }\n@Main function main() { return }");
        assert_eq!(result.tree.items.len(), 2);
        assert!(matches!(result.tree.items[0], Item::Unsupported(_)));
        assert!(matches!(result.tree.items[1], Item::Function(_)));
        assert!(result.diagnostics.iter().any(|d| d.code == Some("KSEM900")));
    }

    #[test]
    fn import_then_function_recovers() {
        let result = parse_text("import Foundation\n@Main function main() { return }");
        assert_eq!(result.tree.items.len(), 2);
        assert!(matches!(result.tree.items[1], Item::Function(_)));
    }

    #[test]
    fn missing_brace_still_terminates() {
        let result = parse_text("function f() { return 1");
        assert!(!result.diagnostics.is_empty());
        assert_eq!(result.tree.items.len(), 1);
    }

    // ----- newline-boundary parity (verified against the reference) -------
    //
    // The reference implementation treats newlines as insignificant inside a
    // function body: expressions continue across lines and `return` attaches a
    // next-line value. Verified by running the reference on scratch packages:
    //   `let a = 5` / `-2` / `print(a)`  -> prints 3   (continuation)
    //   `return` / `42` in an Int fn     -> returns 42 (value attaches)
    //   `return` / `print(..)` in a Void fn -> rejected (value attaches, Void
    //    functions cannot return one)
    // These tests lock that parity so a future "newline ends the statement"
    // change cannot land silently.

    fn function_body(result: &ParseResult) -> &Function {
        match &result.tree.items[0] {
            Item::Function(function) => function,
            other => panic!("expected function, got {other:?}"),
        }
    }

    #[test]
    fn binary_expression_continues_across_a_newline() {
        let result = parse_text("function f() -> Int {\n    let a = 5\n    -2\n    return a\n}");
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        let function = function_body(&result);
        // `5` and `-2` fold into one initializer: exactly two statements.
        assert_eq!(function.body.stmts.len(), 2);
        let kira_syntax_model::ast::Stmt::Let { init, .. } =
            result.tree.stmt(function.body.stmts[0])
        else {
            panic!("expected let");
        };
        assert!(matches!(
            result.tree.expr(*init),
            kira_syntax_model::ast::Expr::Binary {
                op: kira_syntax_model::ast::BinaryOp::Sub,
                ..
            }
        ));
    }

    #[test]
    fn return_attaches_a_next_line_value() {
        let result = parse_text("function f() -> Int {\n    return\n    42\n}");
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        let function = function_body(&result);
        assert_eq!(function.body.stmts.len(), 1);
        assert!(matches!(
            result.tree.stmt(function.body.stmts[0]),
            kira_syntax_model::ast::Stmt::Return { value: Some(_), .. }
        ));
    }

    #[test]
    fn parenthesized_expression_spans_lines() {
        let result = parse_text(
            "function f() -> Int {\n    let a = (1 +\n        2 +\n        3)\n    return a\n}",
        );
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        let function = function_body(&result);
        assert_eq!(function.body.stmts.len(), 2);
    }
}
