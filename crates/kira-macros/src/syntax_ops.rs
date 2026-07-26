//! The declaration-shaped `Syntax` operations: `dropField`, `rewriteProperty`,
//! `replaceIdentifier`, and `identifiers`.
//!
//! All four are span edits over a declaration's original source, so untouched
//! source survives byte-for-byte, comments included. `rewriteProperty` is the
//! one with real work in it: it walks every member body with lexical-scope
//! tracking so a *read* of the property is rewritten only where the property is
//! not shadowed by a local binding, a parameter, a closure parameter, a `for`
//! binding, or a `match` binding.

use kira_source::SourceId;
use kira_syntax_model::TokenKind;

use crate::decl;
use crate::edits::EditBuffer;
use crate::tokens::Lexed;

/// Why a `Syntax` operation could not be performed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SyntaxError {
    /// The value is not a declaration (KMAC026).
    NotADeclaration,
    /// The declaration has no field with that name (KMAC025).
    NoSuchField(String),
    /// An assignment writes *through* the rewritten property (KMAC027).
    WriteThroughProperty(String),
}

/// Every identifier in `text`, in order.
pub(crate) fn identifiers(text: &str) -> Vec<String> {
    let file = Lexed::new(SourceId::new(0), text);
    (0..file.len())
        .filter(|&index| file.is_ident(index))
        .map(|index| file.text_at(index).to_owned())
        .collect()
}

/// Removes the field declaration named `name` from the declaration `text`.
pub(crate) fn drop_field(text: &str, name: &str) -> Result<String, SyntaxError> {
    let declaration = decl::parse(text).ok_or(SyntaxError::NotADeclaration)?;
    let field = declaration
        .fields
        .iter()
        .find(|field| field.name == name)
        .ok_or_else(|| SyntaxError::NoSuchField(name.to_owned()))?;
    let mut buffer = EditBuffer::new();
    buffer.blank(field.span, text);
    Ok(buffer.apply(text).text)
}

/// Rewrites every unshadowed use of the property `name` in the declaration
/// `text`.
///
/// A read becomes `read`; a write `name = value` becomes
/// `write_callee(value)`. An assignment *through* the property — `name.x = v`
/// or `name[i] = v` — has no place to write through and is refused.
pub(crate) fn rewrite_property(
    text: &str,
    name: &str,
    read: &str,
    write_callee: &str,
) -> Result<String, SyntaxError> {
    if decl::parse(text).is_none() {
        return Err(SyntaxError::NotADeclaration);
    }
    let file = Lexed::new(SourceId::new(0), text);
    let mut buffer = EditBuffer::new();
    let mut scopes = Scopes::new();
    let mut index = 0usize;
    while index < file.len() {
        match file.kind(index) {
            TokenKind::Eof => break,
            TokenKind::LBrace => {
                scopes.open();
                index += 1;
                continue;
            }
            TokenKind::RBrace => {
                scopes.close();
                index += 1;
                continue;
            }
            _ => {}
        }
        if let Some(next) = scopes.record_binding(&file, index) {
            index = next;
            continue;
        }
        if !file.is_ident(index) || file.text_at(index) != name {
            index += 1;
            continue;
        }
        if index > 0 && file.kind(index - 1) == TokenKind::Dot {
            index += 1;
            continue;
        }
        if scopes.is_shadowed(name) {
            index += 1;
            continue;
        }
        // A path *through* the property that is then written to: the proxy has
        // nowhere to put the value.
        if matches!(file.kind(index + 1), TokenKind::Dot | TokenKind::LBracket) {
            if let Some(end) = path_end(&file, index)
                && file.kind(end + 1) == TokenKind::Equals
            {
                return Err(SyntaxError::WriteThroughProperty(name.to_owned()));
            }
            buffer.replace(file.span(index), read);
            index += 1;
            continue;
        }
        if file.kind(index + 1) == TokenKind::Equals {
            let value_end = statement_end(&file, index + 2);
            buffer.replace(file.span_of(index, index + 1), format!("{write_callee}("));
            buffer.insert(file.span(value_end).end(), ")");
            index += 2;
            continue;
        }
        buffer.replace(file.span(index), read);
        index += 1;
    }
    Ok(buffer.apply(text).text)
}

/// The last token index of the path starting at `from` (`a.b[0].c`).
fn path_end(file: &Lexed<'_>, from: usize) -> Option<usize> {
    let mut index = from;
    loop {
        match file.kind(index + 1) {
            TokenKind::Dot if file.is_ident(index + 2) => index += 2,
            TokenKind::LBracket => index = file.match_close(index + 1)?,
            _ => return Some(index),
        }
    }
}

/// The last token index of the expression starting at `from`, which runs to the
/// end of its statement.
fn statement_end(file: &Lexed<'_>, from: usize) -> usize {
    let mut last = from;
    let mut index = from;
    while index < file.len() {
        match file.kind(index) {
            TokenKind::Eof | TokenKind::Semicolon | TokenKind::RBrace => break,
            TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => {
                match file.match_close(index) {
                    Some(close) => {
                        last = close;
                        index = close + 1;
                        continue;
                    }
                    None => break,
                }
            }
            _ => {}
        }
        if index > from && file.newline_before(index) {
            break;
        }
        last = index;
        index += 1;
    }
    last
}

/// The scope-stack depth at which a binding is a local rather than a field.
///
/// One scope is open before the first token; the declaration's own `{` opens
/// the second; a member body's `{` opens the third.
const MEMBER_BODY_DEPTH: usize = 3;

/// The lexical scopes a property rewrite has to respect.
///
/// Bindings found before a block are *pending*: a `for` variable, a parameter
/// list, and a closure's parameters all belong to the block that follows them,
/// not to the block they were written in.
struct Scopes {
    stack: Vec<Vec<String>>,
    pending: Vec<String>,
}

impl Scopes {
    fn new() -> Self {
        Self {
            stack: vec![Vec::new()],
            pending: Vec::new(),
        }
    }

    fn open(&mut self) {
        self.stack.push(std::mem::take(&mut self.pending));
    }

    fn close(&mut self) {
        if self.stack.len() > 1 {
            self.stack.pop();
        }
    }

    fn bind(&mut self, name: &str) {
        if let Some(scope) = self.stack.last_mut() {
            scope.push(name.to_owned());
        }
    }

    fn is_shadowed(&self, name: &str) -> bool {
        self.stack
            .iter()
            .any(|scope| scope.iter().any(|bound| bound == name))
    }

    /// Records the binding at `index`, returning the index to continue from
    /// when one was found.
    fn record_binding(&mut self, file: &Lexed<'_>, index: usize) -> Option<usize> {
        match file.kind(index) {
            TokenKind::Let | TokenKind::Var if file.is_ident(index + 1) => {
                // A `let`/`var` in the declaration's own body is a *field*, not
                // a local: it is the property being rewritten, so it shadows
                // nothing. Only a binding inside a member body does. Either
                // way the bound name itself is stepped over rather than
                // rewritten.
                if self.stack.len() >= MEMBER_BODY_DEPTH {
                    self.bind(file.text_at(index + 1));
                }
                Some(index + 2)
            }
            TokenKind::For if file.is_ident(index + 1) => {
                self.pending.push(file.text_at(index + 1).to_owned());
                Some(index + 2)
            }
            TokenKind::Function if file.is_ident(index + 1) => {
                let open = index + 2;
                let close = file.match_close(open)?;
                for (first, _) in file.split_group(open, close) {
                    if file.is_ident(first) {
                        self.pending.push(file.text_at(first).to_owned());
                    }
                }
                Some(close + 1)
            }
            // A `match` arm binds its payload: `Label(text) -> …`.
            TokenKind::Identifier
                if file.kind(index + 1) == TokenKind::LParen
                    && file.is_ident(index + 2)
                    && file.kind(index + 3) == TokenKind::RParen
                    && file.kind(index + 4) == TokenKind::Arrow =>
            {
                self.pending.push(file.text_at(index + 2).to_owned());
                Some(index + 4)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FORM: &str = "MwsPanel MwsDemo() {\n    @MwsState var count: Int = 7\n    let body: Int = 1\n\n    function poke() -> Int {\n        count = count + 1\n        return count\n    }\n\n    function shadowed() -> Int {\n        let count = 99\n        return count\n    }\n}\n";

    #[test]
    fn dropping_a_field_leaves_every_other_byte_alone() {
        let dropped = drop_field(FORM, "count").expect("the field");
        assert!(!dropped.contains("@MwsState"), "{dropped}");
        assert!(dropped.contains("let body: Int = 1"), "{dropped}");
        assert_eq!(dropped.len(), FORM.len());
    }

    #[test]
    fn dropping_a_missing_field_is_refused() {
        assert_eq!(
            drop_field(FORM, "nope"),
            Err(SyntaxError::NoSuchField("nope".to_owned()))
        );
    }

    #[test]
    fn a_non_declaration_is_refused() {
        assert_eq!(
            drop_field("let x = 1", "x"),
            Err(SyntaxError::NotADeclaration)
        );
    }

    #[test]
    fn reads_and_writes_are_rerouted_and_shadowing_is_respected() {
        let rewritten =
            rewrite_property(FORM, "count", "get_count()", "set_count").expect("a rewrite");
        assert!(
            rewritten.contains("set_count( get_count() + 1)"),
            "{rewritten}"
        );
        assert!(rewritten.contains("return get_count()"), "{rewritten}");
        assert!(rewritten.contains("let count = 99"), "{rewritten}");
        assert!(
            rewritten.contains("        return count\n    }\n}"),
            "{rewritten}"
        );
    }

    #[test]
    fn a_member_named_the_same_is_untouched() {
        let text = "struct S {\n    var a: Int\n    function f() -> Int {\n        return other.count\n    }\n}\n";
        let rewritten = rewrite_property(text, "count", "get()", "set").expect("a rewrite");
        assert!(rewritten.contains("other.count"), "{rewritten}");
    }

    #[test]
    fn writing_through_the_property_is_refused() {
        let text = "struct S {\n    var a: Int\n    function f() {\n        count.x = 1\n        return\n    }\n}\n";
        assert_eq!(
            rewrite_property(text, "count", "get()", "set"),
            Err(SyntaxError::WriteThroughProperty("count".to_owned()))
        );
    }

    #[test]
    fn identifiers_are_listed_in_order() {
        assert_eq!(
            identifiers("Read, Write, Execute"),
            vec!["Read", "Write", "Execute"]
        );
    }
}
