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

mod aggregate;
mod construct;
mod expr;
mod generics;
mod item;
mod stmt;
#[cfg(test)]
mod tests;

use kira_core::{Interner, Names, Symbol};
use kira_diagnostics::{Code, Diagnostic, Label, Severity};
use kira_source::{FileSpan, SourceId, Span};
use kira_syntax_model::{FileNodes, FilePart, NodeBase, SyntaxTree, Token, TokenKind};
use std::sync::Arc;

/// The result of parsing a program.
#[derive(Debug, Clone)]
pub struct ParseResult {
    /// The parsed syntax tree (always produced, possibly with error nodes).
    pub tree: SyntaxTree,
    /// What every identifier symbol referenced by the tree stands for.
    pub interner: Names,
    /// Diagnostics produced while parsing.
    pub diagnostics: Vec<Diagnostic>,
}

/// The result of parsing **one** file, ready to be assembled into a program.
///
/// The unit of reuse: a file is parsed against the base its position gives it,
/// so a compilation that finds this answer already computed assembles it
/// straight in rather than parsing the file again.
#[derive(Debug, Clone, PartialEq)]
pub struct FileParse {
    /// The file's items and nodes, numbered into the program.
    pub part: FilePart,
    /// The names this file interned, in the order its symbols name them.
    ///
    /// The deduplicating map is dropped once the file is parsed: it answered
    /// the only question it was for, and keeping it would make holding a
    /// parsed file cost as much as parsing it again.
    pub names: Names,
    /// The base the next file must be parsed against.
    pub end: NodeBase,
    /// The first symbol the next file must number from.
    pub symbol_end: u32,
    /// Diagnostics produced while lexing and parsing this file.
    pub diagnostics: Vec<Diagnostic>,
}

/// Lexes and parses `text`, attributing spans to `source`.
///
/// This is the single entry point for a one-file program; it runs the lexer and
/// then the parser, merging their diagnostics in source order.
#[must_use]
pub fn parse(source: SourceId, text: &str) -> ParseResult {
    assemble(&[parse_file(source, text, NodeBase::default(), 0)])
}

/// Lexes and parses one file, numbering its handles from `base` and its symbols
/// from `symbol_base`.
///
/// A file is parsed **on its own**: nothing about it depends on the contents of
/// any other file, only on how much of the program's id space precedes it. That
/// is what makes this answer worth memoizing — a dependency whose bytes and
/// position have not changed is parsed once per session rather than once per
/// compilation.
#[must_use]
pub fn parse_file(source: SourceId, text: &str, base: NodeBase, symbol_base: u32) -> FileParse {
    let lexed = kira_lexer::lex(source, text);
    let mut parser = Parser::new(source, text, lexed.tokens, base, symbol_base);
    parser.diagnostics = lexed.diagnostics;
    parser.parse_program()
}

/// Assembles parsed files into one program, in the order they were parsed.
///
/// Imports are file-scoped, but a program is one thing: the analyzer resolves
/// names across every file at once, so every file's handles are numbered into
/// one program-wide space and every file's names into one table. Which file an
/// item came from is not lost — [`SyntaxTree::items_with_source`] carries it,
/// and that is what the file-scoped import gate reads.
///
/// Callers pass dependencies before dependents, because a struct field may only
/// name a struct declared earlier.
#[must_use]
pub fn assemble<'a>(files: impl IntoIterator<Item = &'a FileParse>) -> ParseResult {
    let mut interner = Names::new();
    let mut diagnostics = Vec::new();
    let mut parts = Vec::new();
    for file in files {
        interner.append(&file.names);
        diagnostics.extend(file.diagnostics.iter().cloned());
        parts.push(file.part.clone());
    }
    ParseResult {
        tree: SyntaxTree::assemble(parts),
        interner,
        diagnostics,
    }
}

/// Lexes and parses several files into one program.
///
/// The whole-program convenience: every file is parsed against the base the one
/// before it ended at, and the results are assembled. A caller that wants to
/// reuse an unchanged file's answer calls [`parse_file`] and [`assemble`]
/// itself.
#[must_use]
pub fn parse_files(files: &[(SourceId, &str)]) -> ParseResult {
    let mut base = NodeBase::default();
    let mut symbol_base = 0;
    let mut parsed = Vec::with_capacity(files.len());
    for &(source, text) in files {
        let file = parse_file(source, text, base, symbol_base);
        base = file.end;
        symbol_base = file.symbol_end;
        parsed.push(file);
    }
    assemble(&parsed)
}

/// The parser's mutable working state.
struct Parser<'a> {
    source: SourceId,
    text: &'a str,
    tokens: Vec<Token>,
    pos: usize,
    items: Vec<kira_syntax_model::Item>,
    tree: FileNodes,
    interner: Interner,
    diagnostics: Vec<Diagnostic>,
    /// Whether a `{` at expression position opens a block rather than a struct
    /// literal. Set only while parsing an `if`/`while` condition.
    no_struct_literal: bool,
    /// Whether a `name:` following a construction binds to that construction as
    /// a named child fill. Cleared in the value positions where the same tokens
    /// belong to an enclosing initializer instead.
    no_named_fill: bool,
}

impl<'a> Parser<'a> {
    fn new(
        source: SourceId,
        text: &'a str,
        tokens: Vec<Token>,
        base: NodeBase,
        symbol_base: u32,
    ) -> Self {
        Self {
            source,
            text,
            tokens,
            pos: 0,
            items: Vec::new(),
            tree: FileNodes::new(base),
            interner: Interner::with_base(symbol_base),
            diagnostics: Vec::new(),
            no_struct_literal: false,
            no_named_fill: false,
        }
    }

    fn parse_program(mut self) -> FileParse {
        while !self.at_eof() {
            let before = self.pos;
            self.parse_item();
            // Safety net: recovery must always make progress.
            if self.pos == before {
                self.pos += 1;
            }
        }
        FileParse {
            end: self.tree.end(),
            part: FilePart {
                source: self.source,
                items: Arc::from(self.items),
                nodes: Arc::new(self.tree),
            },
            symbol_end: self.interner.next_base(),
            names: self.interner.into_names(),
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

    /// The token `offset` places ahead of the cursor, clamped to the `Eof`
    /// token so lookahead past the end is always a real token rather than a
    /// panic.
    fn peek(&self, offset: usize) -> Token {
        self.tokens[(self.pos + offset).min(self.tokens.len() - 1)]
    }

    /// Whether the token `offset` ahead is the identifier `word`.
    ///
    /// This is how every *contextual* identifier is recognized: `borrow`,
    /// `mut`, `move`, and `copy` are ordinary identifiers to the lexer, so
    /// they are matched by text at the position that gives them meaning and
    /// stay usable as names everywhere else.
    fn peek_is_word(&self, offset: usize, word: &str) -> bool {
        let token = self.peek(offset);
        token.kind == TokenKind::Identifier && self.text_of(token.span) == word
    }

    /// Whether the cursor is on the contextual identifier `word`.
    fn at_word(&self, word: &str) -> bool {
        self.peek_is_word(0, word)
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
        self.intern_text(&text, span)
    }

    /// Interns text the parser assembled rather than sliced — a dotted type
    /// name is spelled across several tokens — reporting a full interner at
    /// `span` exactly as [`Parser::intern_span`] does.
    fn intern_text(&mut self, text: &str, span: Span) -> Symbol {
        match self.interner.intern(text) {
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
        diagnostic.code = Some(Code::known(code));
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

    /// Runs `body` with named child fills disabled, for a value position whose
    /// enclosing initializer list separates its entries by nothing.
    ///
    /// `Style { primary: Color { } secondary: Color { } }` is the case: without
    /// this, `secondary:` would attach to the `Color` just closed rather than
    /// opening the next field of the literal.
    fn without_named_fills<T>(&mut self, body: impl FnOnce(&mut Self) -> T) -> T {
        let saved = self.no_named_fill;
        self.no_named_fill = true;
        let value = body(self);
        self.no_named_fill = saved;
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

    /// The kind of the token already consumed, or [`TokenKind::Eof`] at the
    /// start of the file.
    fn previous_kind(&self) -> TokenKind {
        if self.pos == 0 {
            TokenKind::Eof
        } else {
            self.tokens[self.pos - 1].kind
        }
    }
}
