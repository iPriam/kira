//! A token cursor over one file, plus the balanced-delimiter and
//! statement-boundary questions every scan in this crate asks.
//!
//! Macro expansion is a source-to-source transform, so it reads the file
//! through tokens (which already skip comments and understand string literals)
//! but always writes *byte ranges of the original text*. Nothing here parses:
//! it locates.

use kira_lexer::lex;
use kira_source::{SourceId, Span};
use kira_syntax_model::{Token, TokenKind};

/// One lexed file: its id, its text, and its tokens.
pub(crate) struct Lexed<'a> {
    /// The file the tokens came from.
    pub(crate) source: SourceId,
    /// The file's full text.
    pub(crate) text: &'a str,
    /// The token stream, always ending in [`TokenKind::Eof`].
    pub(crate) tokens: Vec<Token>,
}

impl<'a> Lexed<'a> {
    /// Lexes `text`, discarding the lexer's own diagnostics.
    ///
    /// The diagnostics are discarded deliberately: this is a *pre-pass*, and
    /// the same text (or its expansion) is lexed again by the real frontend,
    /// which reports them once. Reporting here would double every stray byte.
    pub(crate) fn new(source: SourceId, text: &'a str) -> Self {
        Self {
            source,
            text,
            tokens: lex(source, text).tokens,
        }
    }

    /// The number of tokens, `Eof` included.
    pub(crate) fn len(&self) -> usize {
        self.tokens.len()
    }

    /// The kind at `index`, or [`TokenKind::Eof`] past the end.
    pub(crate) fn kind(&self, index: usize) -> TokenKind {
        self.tokens
            .get(index)
            .map_or(TokenKind::Eof, |token| token.kind)
    }

    /// The span at `index`, or an empty span at end of file past the end.
    pub(crate) fn span(&self, index: usize) -> Span {
        self.tokens
            .get(index)
            .map_or_else(|| Span::new(self.text.len() as u32, 0), |token| token.span)
    }

    /// The source text the token at `index` covers.
    pub(crate) fn text_at(&self, index: usize) -> &'a str {
        self.slice(self.span(index))
    }

    /// The source text `span` covers.
    pub(crate) fn slice(&self, span: Span) -> &'a str {
        let start = (span.start as usize).min(self.text.len());
        let end = (span.end() as usize).min(self.text.len());
        self.text.get(start..end).unwrap_or("")
    }

    /// Whether the token at `index` is the identifier `word`.
    ///
    /// `macro`, `comptime`, `expand`, and `quote` are *contextual*: the lexer
    /// hands them back as ordinary identifiers, so every one of them is matched
    /// by text at the position that gives it meaning and stays usable as a name
    /// everywhere else.
    pub(crate) fn is_word(&self, index: usize, word: &str) -> bool {
        self.kind(index) == TokenKind::Identifier && self.text_at(index) == word
    }

    /// Whether the token at `index` is an identifier at all.
    pub(crate) fn is_ident(&self, index: usize) -> bool {
        self.kind(index) == TokenKind::Identifier
    }

    /// The index of the delimiter closing the one at `open`, or `None` when the
    /// file ends first.
    ///
    /// `open` must sit on `(`, `[`, or `{`; anything else has no match and
    /// returns `None` rather than scanning to the end.
    pub(crate) fn match_close(&self, open: usize) -> Option<usize> {
        let (opener, closer) = match self.kind(open) {
            TokenKind::LParen => (TokenKind::LParen, TokenKind::RParen),
            TokenKind::LBracket => (TokenKind::LBracket, TokenKind::RBracket),
            TokenKind::LBrace => (TokenKind::LBrace, TokenKind::RBrace),
            _ => return None,
        };
        let mut depth = 0usize;
        let mut index = open;
        while index < self.len() {
            let kind = self.kind(index);
            if kind == opener {
                depth += 1;
            } else if kind == closer {
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
            } else if kind == TokenKind::Eof {
                return None;
            }
            index += 1;
        }
        None
    }

    /// The index of the delimiter opening the one at `close`, or `None` when
    /// the file starts first.
    ///
    /// The mirror of [`Lexed::match_close`], for the scans that walk backwards
    /// out of an expression towards the statement containing it.
    pub(crate) fn match_open(&self, close: usize) -> Option<usize> {
        let (opener, closer) = match self.kind(close) {
            TokenKind::RParen => (TokenKind::LParen, TokenKind::RParen),
            TokenKind::RBracket => (TokenKind::LBracket, TokenKind::RBracket),
            TokenKind::RBrace => (TokenKind::LBrace, TokenKind::RBrace),
            _ => return None,
        };
        let mut depth = 0usize;
        let mut index = close;
        loop {
            let kind = self.kind(index);
            if kind == closer {
                depth += 1;
            } else if kind == opener {
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
            }
            if index == 0 {
                return None;
            }
            index -= 1;
        }
    }

    /// The span running from the start of token `first` to the end of token
    /// `last`.
    pub(crate) fn span_of(&self, first: usize, last: usize) -> Span {
        Span::from_bounds(self.span(first).start, self.span(last).end())
    }

    /// Whether a line break separates the token at `index` from the one before
    /// it.
    ///
    /// Kira separates statements with `;` **or** a newline. The parser recovers
    /// statement boundaries structurally and so never needs to ask, but macro
    /// expansion does: whether `name!(…)` sits in statement or expression
    /// position is exactly the question of whether a statement started at it.
    pub(crate) fn newline_before(&self, index: usize) -> bool {
        if index == 0 {
            return true;
        }
        let previous_end = self.span(index - 1).end() as usize;
        let start = self.span(index).start as usize;
        let between = self
            .text
            .get(previous_end.min(self.text.len())..start.min(self.text.len()))
            .unwrap_or("");
        between.contains('\n')
    }

    /// The indices of the top-level commas inside the group opening at `open`,
    /// in order.
    ///
    /// "Top level" means at the group's own nesting depth: a comma inside a
    /// nested `(…)`, `[…]`, or `{…}` belongs to that group, not this one.
    pub(crate) fn top_level_commas(&self, open: usize, close: usize) -> Vec<usize> {
        let mut commas = Vec::new();
        let mut index = open + 1;
        while index < close {
            match self.kind(index) {
                TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => {
                    match self.match_close(index) {
                        Some(end) => index = end,
                        None => break,
                    }
                }
                TokenKind::Comma => commas.push(index),
                _ => {}
            }
            index += 1;
        }
        commas
    }

    /// Splits the group `open..=close` at its top-level commas, returning one
    /// `(first, last)` token-index pair per non-empty element.
    ///
    /// An element with no tokens — a trailing comma, or an empty argument list
    /// — contributes nothing, so `f!()` has zero arguments rather than one
    /// empty one.
    pub(crate) fn split_group(&self, open: usize, close: usize) -> Vec<(usize, usize)> {
        let mut parts = Vec::new();
        let mut start = open + 1;
        for comma in self
            .top_level_commas(open, close)
            .into_iter()
            .chain([close])
        {
            if comma > start {
                parts.push((start, comma - 1));
            }
            start = comma + 1;
        }
        parts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lexed(text: &str) -> Lexed<'_> {
        Lexed::new(SourceId::new(0), text)
    }

    #[test]
    fn matches_nested_delimiters() {
        let file = lexed("f(a(b), c)");
        assert_eq!(file.match_close(1), Some(8));
        assert_eq!(file.match_close(3), Some(5));
    }

    #[test]
    fn splits_arguments_at_top_level_commas_only() {
        let file = lexed("f(a(1, 2), b)");
        let close = file.match_close(1).expect("a closing paren");
        let parts = file.split_group(1, close);
        let texts: Vec<&str> = parts
            .iter()
            .map(|&(first, last)| file.slice(file.span_of(first, last)))
            .collect();
        assert_eq!(texts, vec!["a(1, 2)", "b"]);
    }

    #[test]
    fn an_empty_group_has_no_elements() {
        let file = lexed("f()");
        let close = file.match_close(1).expect("a closing paren");
        assert!(file.split_group(1, close).is_empty());
    }

    #[test]
    fn a_newline_marks_a_statement_boundary() {
        let file = lexed("var r = 0\nsomething()");
        // tokens: var r = 0 something ( )
        assert!(file.newline_before(4));
        assert!(!file.newline_before(3));
    }

    #[test]
    fn contextual_words_are_matched_by_text() {
        let file = lexed("comptime macro Name {}");
        assert!(file.is_word(0, "comptime"));
        assert!(file.is_word(1, "macro"));
        assert!(!file.is_word(1, "comptime"));
    }
}
