//! A declaration's body, as statements a macro can walk.
//!
//! Everything else in the reflection API is a *locating scan* over tokens (see
//! [`crate::decl`]), because everything else is about a declaration's shape:
//! its name, its fields, the text it covers. A body is different — the
//! questions worth asking about one are structural (*is this `while` counting
//! an index by hand?*, *does this `match` end in a catch-all?*), and a token
//! scan cannot answer those without becoming a parser badly.
//!
//! So this reuses the real parser. The declaration's own source text is a
//! complete declaration, so parsing it needs no wrapper and no reconstruction.
//!
//! # Spans survive the round trip
//!
//! Parsing detached text yields offsets into *that text*, which point nowhere
//! in any file. They are recoverable because `Declaration.syntax` is the exact
//! source slice the declaration covers: adding the declaration's own start to a
//! local offset lands on the same byte in the real file. That is what lets a
//! lint reporting a statement underline the statement rather than the whole
//! declaration.

use std::sync::Arc;

use kira_source::{FileSpan, SourceId, Span};
use kira_syntax_model::SyntaxTree;
use kira_syntax_model::ast::{Block, Item, Stmt, StmtId};

/// One statement inside a declaration's body.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Statement {
    /// Which statement form it is: `let`, `assign`, `return`, `expr`, `if`,
    /// `while`, `for`, `match`, `attempt`, `break`, `continue`.
    ///
    /// A word rather than an enum for the same reason `Declaration.kind` is
    /// one: a macro body compares it to a string, and the set is the compiler's
    /// to grow.
    pub(crate) kind: &'static str,
    /// The statement's exact source text.
    pub(crate) syntax: String,
    /// Where it was written, when the declaration knew where it came from.
    pub(crate) span: Option<FileSpan>,
    /// The same span, as an offset into [`Statement::text`].
    ///
    /// Kept beside the file span because a *rewrite* needs the source it is
    /// rewriting: a lint replacing a run of statements has to read what is
    /// between them, and only the declaration's own text has that.
    pub(crate) local: Span,
    /// The declaration's whole source text, shared by every statement in it.
    pub(crate) text: Arc<str>,
    /// The head a reader branches on: an `if`/`while` condition, a `for`'s
    /// iterable, a `match`'s subject. Empty for a statement with no head.
    pub(crate) head: String,
    /// The statements directly inside it — a block's contents, both halves of
    /// an `if`, every arm of a `match`. Empty for a statement with no body.
    pub(crate) body: Vec<Statement>,
}

/// Reads the statements of `syntax`, a declaration's exact source text.
///
/// `at` is where that text starts in its file, when it came from one; every
/// statement's span is offset by it. `None` in, `None` on every statement out.
///
/// An empty result covers every declaration with no body — a struct, an enum, a
/// construct family — and also one whose body did not parse, which is a
/// question for the real frontend rather than something to report twice.
pub(crate) fn statements_of(syntax: &str, at: Option<FileSpan>) -> Vec<Statement> {
    let parsed = kira_parser::parse(SourceId::new(0), syntax);
    let Some(Item::Function(function)) = parsed.tree.items().first() else {
        return Vec::new();
    };
    let reader = Reader {
        tree: &parsed.tree,
        text: syntax,
        shared: Arc::from(syntax),
        at,
    };
    reader.block(&function.body)
}

/// Walks one parsed body, carrying what every statement needs to describe
/// itself.
struct Reader<'a> {
    tree: &'a SyntaxTree,
    text: &'a str,
    shared: Arc<str>,
    at: Option<FileSpan>,
}

impl Reader<'_> {
    /// Every statement of `block`, in source order.
    fn block(&self, block: &Block) -> Vec<Statement> {
        block.stmts.iter().map(|&id| self.statement(id)).collect()
    }

    /// One statement, with its head and its nested statements.
    fn statement(&self, id: StmtId) -> Statement {
        let stmt = self.tree.stmt(id);
        let span = stmt.span();
        Statement {
            kind: kind_of(stmt),
            syntax: self.slice(span),
            span: self.file_span(span),
            local: span,
            text: Arc::clone(&self.shared),
            head: self.head_of(stmt),
            body: self.body_of(stmt),
        }
    }

    /// The text a span covers, or the empty string when it runs off the end.
    fn slice(&self, span: Span) -> String {
        let start = span.start as usize;
        let end = span.end() as usize;
        self.text.get(start..end).unwrap_or("").to_owned()
    }

    /// A local span rebased onto the file the declaration came from.
    fn file_span(&self, span: Span) -> Option<FileSpan> {
        let at = self.at?;
        Some(FileSpan::new(
            at.source,
            Span::new(at.span.start + span.start, span.len),
        ))
    }

    /// The expression a statement branches on, as written.
    fn head_of(&self, stmt: &Stmt) -> String {
        let expr = match stmt {
            Stmt::If { cond, .. } | Stmt::While { cond, .. } => *cond,
            Stmt::Match { subject, .. } => *subject,
            _ => return String::new(),
        };
        self.slice(self.tree.expr(expr).span())
    }

    /// The statements directly inside a statement.
    fn body_of(&self, stmt: &Stmt) -> Vec<Statement> {
        match stmt {
            Stmt::While { body, .. } | Stmt::For { body, .. } => self.block(body),
            // Both halves, in written order: a lint asking "what does this `if`
            // do" means all of it, and keeping them separate would make every
            // caller re-join them.
            Stmt::If {
                then_block,
                else_block,
                ..
            } => {
                let mut all = self.block(then_block);
                if let Some(otherwise) = else_block {
                    all.extend(self.block(otherwise));
                }
                all
            }
            Stmt::Match { arms, .. } => arms.iter().flat_map(|arm| self.block(&arm.body)).collect(),
            Stmt::Attempt { body, handlers, .. } => {
                let mut all = self.block(body);
                for handler in handlers {
                    all.extend(self.block(&handler.body));
                }
                all
            }
            _ => Vec::new(),
        }
    }
}

/// The word a statement form is named by.
fn kind_of(stmt: &Stmt) -> &'static str {
    match stmt {
        Stmt::Let { .. } => "let",
        Stmt::Assign { .. } => "assign",
        Stmt::Return { .. } => "return",
        Stmt::Expr { .. } => "expr",
        Stmt::If { .. } => "if",
        Stmt::While { .. } => "while",
        Stmt::For { .. } => "for",
        Stmt::Match { .. } => "match",
        Stmt::Attempt { .. } => "attempt",
        Stmt::Break { .. } => "break",
        Stmt::Continue { .. } => "continue",
        Stmt::Error { .. } => "error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_body_reports_its_statements_in_order() {
        let source = "function f() {\n    let x = 1\n    x = 2\n    return x\n}\n";
        let body = statements_of(source, None);
        let kinds: Vec<&str> = body.iter().map(|s| s.kind).collect();
        assert_eq!(kinds, vec!["let", "assign", "return"]);
        assert_eq!(body[1].syntax, "x = 2");
    }

    #[test]
    fn a_loop_reports_its_condition_and_its_contents() {
        let source = "function f() {\n    while i < xs.count {\n        total = total + 1\n        i = i + 1\n    }\n}\n";
        let body = statements_of(source, None);
        assert_eq!(body[0].kind, "while");
        assert_eq!(body[0].head, "i < xs.count");
        let inner: Vec<&str> = body[0].body.iter().map(|s| s.kind).collect();
        assert_eq!(inner, vec!["assign", "assign"]);
        // The shape `manual-index-loop` looks for: the last statement of a
        // `while` stepping the same name its condition compares.
        assert_eq!(body[0].body[1].syntax, "i = i + 1");
    }

    #[test]
    fn both_halves_of_an_if_are_reachable() {
        let source = "function f() {\n    if a {\n        return 1\n    } else {\n        return 2\n    }\n}\n";
        let body = statements_of(source, None);
        assert_eq!(body[0].kind, "if");
        assert_eq!(body[0].head, "a");
        assert_eq!(body[0].body.len(), 2, "then and else are both walked");
    }

    #[test]
    fn a_declaration_with_no_body_reports_none() {
        assert!(statements_of("struct Point {\n    var x: Int\n}\n", None).is_empty());
        assert!(statements_of("enum Colour {\n    Red\n}\n", None).is_empty());
        // Text that does not parse is the frontend's to report, not this.
        assert!(statements_of("function f( {", None).is_empty());
    }

    #[test]
    fn a_statement_span_lands_where_the_declaration_does() {
        // A declaration starting at byte 100 of file 3: every statement span is
        // its local offset plus 100, so a lint underlines real bytes.
        let source = "function f() {\n    return 1\n}\n";
        let at = FileSpan::new(SourceId::new(3), Span::new(100, source.len() as u32));
        let body = statements_of(source, Some(at));
        let span = body[0].span.expect("a rebased span");
        assert_eq!(span.source, SourceId::new(3));
        let local = source.find("return 1").expect("the statement") as u32;
        assert_eq!(span.span.start, 100 + local);
    }
}
