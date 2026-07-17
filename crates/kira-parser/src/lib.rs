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
mod item;
mod stmt;

use kira_core::{Interner, Symbol};
use kira_diagnostics::{Diagnostic, Label, Severity};
use kira_source::{FileSpan, SourceId, Span};
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
    /// Whether a `{` at expression position opens a block rather than a struct
    /// literal. Set only while parsing an `if`/`while` condition.
    no_struct_literal: bool,
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
            no_struct_literal: false,
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

    /// Interns the text a span covers.
    ///
    /// An interner with no handles left is reported like any other parse
    /// problem and the name becomes [`Symbol::ERROR`]: this parser is
    /// error-resilient, so it carries on and produces a tree plus a
    /// diagnostic rather than bailing.
    fn intern_span(&mut self, span: Span) -> Symbol {
        let text = span.slice(self.text).to_owned();
        match self.interner.intern(&text) {
            Ok(symbol) => symbol,
            Err(full) => {
                self.error(span, "KPAR030", full.to_string());
                Symbol::ERROR
            }
        }
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

    /// Runs `body` with struct literals disabled, for a position where a `{`
    /// opens a block rather than a literal (an `if`/`while` condition).
    ///
    /// Newlines are insignificant here, so `if p { … }` is genuinely ambiguous
    /// between a condition `p` followed by a block and a literal `p { … }`.
    /// The block always wins, and a literal in that position is written with
    /// parentheses — the same rule Rust settled on.
    fn without_struct_literals<T>(&mut self, body: impl FnOnce(&mut Self) -> T) -> T {
        let saved = self.no_struct_literal;
        self.no_struct_literal = true;
        let value = body(self);
        self.no_struct_literal = saved;
        value
    }

    /// Runs `body` with struct literals re-enabled, for a position already
    /// bracketed by a delimiter — inside `(…)`, a call's arguments, or another
    /// literal's fields. The ambiguity a condition has does not reach there.
    fn with_struct_literals<T>(&mut self, body: impl FnOnce(&mut Self) -> T) -> T {
        let saved = self.no_struct_literal;
        self.no_struct_literal = false;
        let value = body(self);
        self.no_struct_literal = saved;
        value
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

#[cfg(test)]
mod tests {
    use super::*;
    use kira_runtime_abi::Execution;
    use kira_syntax_model::ast::{Function, Item};

    fn parse_text(text: &str) -> ParseResult {
        parse(SourceId::new(0), text)
    }

    /// The one function in `text`, for tests that parse a single declaration.
    fn only_function(result: &ParseResult) -> &Function {
        match result.tree.items.as_slice() {
            [Item::Function(function)] => function,
            items => panic!("expected exactly one function, got {items:?}"),
        }
    }

    #[test]
    fn execution_annotations_select_an_engine() {
        let runtime = parse_text("@Runtime function f() { return }");
        assert_eq!(only_function(&runtime).execution, Execution::Runtime);
        assert!(runtime.diagnostics.is_empty());

        let native = parse_text("@Native function f() { return }");
        assert_eq!(only_function(&native).execution, Execution::Native);
        assert!(native.diagnostics.is_empty());
    }

    #[test]
    fn an_unannotated_function_inherits_the_builds_engine() {
        let plain = parse_text("function f() { return }");
        assert_eq!(only_function(&plain).execution, Execution::Inherited);
    }

    #[test]
    fn execution_annotations_compose_with_main() {
        let result = parse_text("@Main @Native function main() { return }");
        let function = only_function(&result);
        assert!(function.is_main);
        assert_eq!(function.execution, Execution::Native);
        assert!(result.diagnostics.is_empty());
    }

    #[test]
    fn two_engines_on_one_function_is_reported() {
        let result = parse_text("@Runtime @Native function f() { return }");
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == Some("KPAR005")),
            "a contradictory engine pair must be reported, not silently resolved",
        );
        // Parsing still yields a usable function: the parser never bails.
        assert_eq!(result.tree.items.len(), 1);
    }

    #[test]
    fn repeating_one_engine_is_not_a_contradiction() {
        let result = parse_text("@Native @Native function f() { return }");
        assert_eq!(only_function(&result).execution, Execution::Native);
        assert!(result.diagnostics.is_empty());
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
        let result = parse_text("enum E { A }\n@Main function main() { return }");
        assert_eq!(result.tree.items.len(), 2);
        assert!(matches!(result.tree.items[0], Item::Unsupported(_)));
        assert!(matches!(result.tree.items[1], Item::Function(_)));
        assert!(result.diagnostics.iter().any(|d| d.code == Some("KSEM900")));
    }

    // ----- structs -------------------------------------------------------

    /// The one struct in `text`, for tests that parse a single declaration.
    fn only_struct(result: &ParseResult) -> &kira_syntax_model::ast::StructDecl {
        match result.tree.items.as_slice() {
            [Item::Struct(declaration)] => declaration,
            items => panic!("expected exactly one struct, got {items:?}"),
        }
    }

    #[test]
    fn parses_a_struct_with_let_and_var_members() {
        let result = parse_text("struct Point {\n    let x: Int\n    var y: Float\n}");
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        let declaration = only_struct(&result);
        assert_eq!(result.interner.resolve(declaration.name), "Point");
        assert_eq!(declaration.fields.len(), 2);
        assert!(!declaration.fields[0].mutable);
        assert!(declaration.fields[1].mutable);
        assert!(declaration.fields[0].default.is_none());
    }

    #[test]
    fn semicolons_separate_members_on_one_line() {
        let result = parse_text("struct Pair { var w: Int = 0; var h: Int = 0 }");
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        let declaration = only_struct(&result);
        assert_eq!(declaration.fields.len(), 2);
        assert!(declaration.fields.iter().all(|f| f.default.is_some()));
    }

    #[test]
    fn an_empty_struct_parses() {
        let result = parse_text("struct Blank {}");
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        assert!(only_struct(&result).fields.is_empty());
    }

    #[test]
    fn a_member_without_let_or_var_is_reported_and_recovers() {
        let result = parse_text("struct P { x: Int\n let y: Int }");
        assert!(result.diagnostics.iter().any(|d| d.code == Some("KPAR009")));
        // Recovery keeps the well-formed member: the parser never bails.
        let declaration = only_struct(&result);
        assert!(
            declaration
                .fields
                .iter()
                .any(|f| result.interner.resolve(f.name) == "y"),
        );
    }

    #[test]
    fn a_struct_method_is_reported_but_members_still_parse() {
        let result = parse_text(
            "struct P {\n let x: Int\n function sum() -> Int { return x }\n let y: Int\n}",
        );
        assert!(
            result.diagnostics.iter().any(|d| d.code == Some("KPAR008")),
            "a method must be reported, not silently dropped",
        );
        let declaration = only_struct(&result);
        assert_eq!(declaration.fields.len(), 2, "{:?}", declaration.fields);
    }

    // ----- struct literals and field access ------------------------------

    /// The first statement of the first function in `text`.
    fn first_stmt(result: &ParseResult) -> &kira_syntax_model::ast::Stmt {
        let function = match &result.tree.items[0] {
            Item::Function(function) => function,
            other => panic!("expected function, got {other:?}"),
        };
        result.tree.stmt(function.body.stmts[0])
    }

    #[test]
    fn parses_a_struct_literal_with_both_binders() {
        let result = parse_text("function f() { let p = Point { x = 1, y: 2 } }");
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        let kira_syntax_model::ast::Stmt::Let { init, .. } = first_stmt(&result) else {
            panic!("expected let");
        };
        let kira_syntax_model::ast::Expr::StructLit { fields, .. } = result.tree.expr(*init) else {
            panic!("expected a struct literal");
        };
        assert_eq!(fields.len(), 2, "both binders normalize to one node");
    }

    #[test]
    fn struct_literal_fields_need_no_separator() {
        // Newlines are insignificant, so a comma cannot be required.
        let result = parse_text("function f() { let p = Point {\n    x = 1\n    y = 2\n} }");
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        let kira_syntax_model::ast::Stmt::Let { init, .. } = first_stmt(&result) else {
            panic!("expected let");
        };
        let kira_syntax_model::ast::Expr::StructLit { fields, .. } = result.tree.expr(*init) else {
            panic!("expected a struct literal");
        };
        assert_eq!(fields.len(), 2);
    }

    #[test]
    fn a_brace_after_a_condition_opens_a_block_not_a_literal() {
        // The ambiguity newline-insignificance creates: `if flag { … }` must
        // read as a condition plus a block, never as a literal `flag { … }`.
        let result = parse_text("function f() { if flag { return } }");
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        let kira_syntax_model::ast::Stmt::If {
            cond, then_block, ..
        } = first_stmt(&result)
        else {
            panic!("expected if");
        };
        assert!(matches!(
            result.tree.expr(*cond),
            kira_syntax_model::ast::Expr::Name { .. }
        ));
        assert_eq!(then_block.stmts.len(), 1);
    }

    #[test]
    fn a_parenthesized_literal_is_still_allowed_in_a_condition() {
        let result = parse_text("function f() { if (Point { x = 1 }).x > 0 { return } }");
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        assert!(matches!(
            first_stmt(&result),
            kira_syntax_model::ast::Stmt::If { .. }
        ));
    }

    #[test]
    fn a_literal_inside_a_condition_call_is_allowed() {
        // The suppression must not leak past a delimiter.
        let result = parse_text("function f() { while check(Point { x = 1 }) { return } }");
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        assert!(matches!(
            first_stmt(&result),
            kira_syntax_model::ast::Stmt::While { .. }
        ));
    }

    #[test]
    fn parses_a_chained_field_read() {
        let result = parse_text("function f() -> Int { return b.size.x }");
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        let kira_syntax_model::ast::Stmt::Return {
            value: Some(id), ..
        } = first_stmt(&result)
        else {
            panic!("expected return");
        };
        let kira_syntax_model::ast::Expr::Field { base, .. } = result.tree.expr(*id) else {
            panic!("expected a field read");
        };
        // Left-associative: `(b.size).x`.
        assert!(matches!(
            result.tree.expr(*base),
            kira_syntax_model::ast::Expr::Field { .. }
        ));
    }

    #[test]
    fn parses_assignment_to_a_local_and_to_a_field_path() {
        let result = parse_text("function f() { x = 1\n b.size.x = 2 }");
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        let function = match &result.tree.items[0] {
            Item::Function(function) => function,
            other => panic!("expected function, got {other:?}"),
        };
        assert_eq!(function.body.stmts.len(), 2);
        for stmt in &function.body.stmts {
            assert!(matches!(
                result.tree.stmt(*stmt),
                kira_syntax_model::ast::Stmt::Assign { .. }
            ));
        }
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
