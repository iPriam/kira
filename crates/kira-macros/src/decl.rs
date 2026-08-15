//! Declaration reflection: what a macro sees when it is handed a
//! `Declaration`.
//!
//! This is a *locating* scan, not a parse. `Declaration.syntax` and
//! `Field.syntax` are documented as the declaration's and the field's exact
//! source text, and `Syntax.dropField` / `Syntax.rewriteProperty` are span
//! edits that must leave everything they do not touch byte-for-byte intact,
//! comments included. So every piece here is a byte range of the original file
//! rather than a node, and the annotations the real parser discards — the ones
//! that summon macros — are preserved.

use kira_source::{FileSpan, SourceId, Span};
use kira_syntax_model::TokenKind;

use crate::tokens::Lexed;

/// Which declaration form a macro was applied to.
///
/// These are the words an `appliesTo { … }` list is written with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeclarationKind {
    /// `struct Name { … }`
    Struct,
    /// `class Name { … }`
    Class,
    /// `enum Name { … }`
    Enum,
    /// `construct Family { … }`
    Construct,
    /// A construct-backed declaration: `Family Name(params) { … }`.
    Form,
    /// `function name(…) { … }`
    Function,
    /// Anything else at file scope.
    Other,
}

impl DeclarationKind {
    /// The `appliesTo` word this kind is written with.
    pub(crate) fn word(self) -> &'static str {
        match self {
            DeclarationKind::Struct => "struct",
            DeclarationKind::Class => "class",
            DeclarationKind::Enum => "enum",
            DeclarationKind::Construct => "construct",
            DeclarationKind::Form => "form",
            DeclarationKind::Function => "function",
            DeclarationKind::Other => "declaration",
        }
    }

    /// The `DeclarationForm` variant a macro body matches this kind as.
    ///
    /// Distinct from [`DeclarationKind::word`], which is the lowercase spelling
    /// an `appliesTo` list is written with. A macro body reads the *variant*,
    /// so `match target.kind { Enum -> … }` is a closed set the evaluator
    /// checks rather than a string nothing checks.
    pub(crate) fn variant(self) -> &'static str {
        match self {
            DeclarationKind::Struct => "Struct",
            DeclarationKind::Class => "Class",
            DeclarationKind::Enum => "Enum",
            DeclarationKind::Construct => "Construct",
            DeclarationKind::Form => "Form",
            DeclarationKind::Function => "Function",
            DeclarationKind::Other => "Declaration",
        }
    }
}

/// One `@Name` or `@Derive(A, B)` written above a declaration or a field.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Annotation {
    /// The annotation's name, without the `@`.
    pub(crate) name: String,
    /// The names inside its `(…)`, in order; empty when it has none.
    pub(crate) arguments: Vec<String>,
    /// The bytes the whole annotation covers, `@` included.
    pub(crate) span: Span,
}

/// One field or enum variant of a declaration.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Field {
    /// The field's name, or the variant's.
    pub(crate) name: String,
    /// The written type, or `""` for a payload-less variant.
    pub(crate) type_text: String,
    /// The initial-value expression as written, or `""` when absent.
    pub(crate) initializer: String,
    /// The whole field declaration, annotations included.
    pub(crate) syntax: String,
    /// The bytes the whole field declaration covers, annotations included.
    pub(crate) span: Span,
    /// The file [`Field::span`] points into, or `None` for a re-scan.
    ///
    /// See [`Declaration::source`] — the two are `None` together and for the
    /// same reason.
    pub(crate) source: Option<SourceId>,
    /// The annotations written above it.
    pub(crate) annotations: Vec<Annotation>,
}

impl Field {
    /// Where the field was written, when that is a real place in a real file.
    pub(crate) fn at(&self) -> Option<FileSpan> {
        self.source.map(|source| FileSpan::new(source, self.span))
    }
}

/// One hook of a construct family's `lifecycle { … }` section.
///
/// A hook marked `@Comptime` runs **during compilation**, once for each
/// declaration backed by the family, with `Self` bound to that declaration. It
/// is what lets a family act on its own declarations without a collector macro
/// standing between them.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Hook {
    /// The hook's name.
    pub(crate) name: String,
    /// The text between the braces of its body.
    pub(crate) body: String,
    /// Whether it carried `@Comptime`.
    pub(crate) comptime: bool,
    /// Where it was written.
    pub(crate) span: Span,
}

/// One behaviour member of a construct-backed declaration.
///
/// The `path { … }` shorthand and the long `function path() -> String { … }` are
/// the same thing here: a name and a body. What a macro does with the body is
/// run it — see `Declaration.value(name)` — which is what lets a family's
/// declarations be read as data during compilation rather than at startup.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Member {
    /// The member's name.
    pub(crate) name: String,
    /// The text between the braces of its body.
    pub(crate) body: String,
}

/// One declaration, as a macro sees it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Declaration {
    /// Which form it wears.
    pub(crate) kind: DeclarationKind,
    /// Its name.
    pub(crate) name: String,
    /// The construct family backing it, for a [`DeclarationKind::Form`].
    ///
    /// Empty for every other kind. This is what lets a macro ask which family a
    /// declaration is written in without the compiler knowing any family by
    /// name — `Test`, `Printable`, or one a program declares itself are all the
    /// same question asked of this string.
    pub(crate) family: String,
    /// Its fields, or an enum's variants, in declaration order.
    pub(crate) fields: Vec<Field>,
    /// Its behaviour members, in declaration order. Empty for a declaration
    /// that has none.
    pub(crate) members: Vec<Member>,
    /// The hooks of its `lifecycle { … }` section, for a family that has one.
    pub(crate) hooks: Vec<Hook>,
    /// The declaration's exact source text, annotations **excluded**.
    pub(crate) syntax: String,
    /// The bytes the declaration covers, annotations excluded.
    pub(crate) span: Span,
    /// The file [`Declaration::span`] points into, or `None` when the span
    /// points nowhere a reader could open.
    ///
    /// [`scan`] fills this in; [`parse`] deliberately does not. A re-scan lexes
    /// a *detached string* — syntax a macro built, or a declaration's own text
    /// handed back — so its byte offsets are relative to that string and mean
    /// nothing in any file. Anchoring a diagnostic there would underline
    /// whatever bytes happen to sit at those offsets in whichever file the id
    /// named, which is worse than declining to point at all.
    pub(crate) source: Option<SourceId>,
    /// The 1-based line [`Declaration::span`] starts on, or `0` for a re-scan.
    pub(crate) line: u32,
    /// The path the file was read from, or `""` when it is not known.
    ///
    /// Shared rather than copied: every declaration in a file names the same
    /// one, and a program has far more declarations than files.
    pub(crate) path: std::sync::Arc<str>,
    /// How many lines the whole file holds, or `0` for a re-scan.
    ///
    /// Counted here rather than resolved later because this is the only place
    /// that holds the file's text: a macro is handed declarations, never files,
    /// so a lint about a file's *size* has nowhere else to read it from.
    pub(crate) file_lines: u32,
    /// The annotations written above it.
    pub(crate) annotations: Vec<Annotation>,
}

impl Declaration {
    /// Where the declaration was written, when that is a real place in a real
    /// file.
    pub(crate) fn at(&self) -> Option<FileSpan> {
        self.source.map(|source| FileSpan::new(source, self.span))
    }
}

/// Scans the declaration starting at token `start`, which must sit on the first
/// `@` or on the declaration's first keyword.
///
/// Returns the declaration and the index just past it, or `None` when the file
/// ends inside it.
pub(crate) fn scan(file: &Lexed<'_>, start: usize) -> Option<(Declaration, usize)> {
    let (annotations, head) = scan_annotations(file, start);
    let (kind, name_index) = match file.kind(head) {
        TokenKind::Struct => (DeclarationKind::Struct, head + 1),
        TokenKind::Class => (DeclarationKind::Class, head + 1),
        TokenKind::Enum => (DeclarationKind::Enum, head + 1),
        TokenKind::Construct => (DeclarationKind::Construct, head + 1),
        TokenKind::Function => (DeclarationKind::Function, head + 1),
        // A construct-backed declaration: `Family Name(params) { … }`, and the
        // parameterless `Family Name { … }` a family with no construction
        // inputs is written with. Both are two identifiers where every other
        // declaration form opens with a keyword.
        TokenKind::Identifier
            if file.is_ident(head + 1)
                && matches!(file.kind(head + 2), TokenKind::LParen | TokenKind::LBrace) =>
        {
            (DeclarationKind::Form, head + 1)
        }
        _ => (DeclarationKind::Other, head),
    };
    let name = if file.is_ident(name_index) {
        file.text_at(name_index).to_owned()
    } else {
        String::new()
    };
    // The family sits where every other form has its keyword.
    let family = if kind == DeclarationKind::Form {
        file.text_at(head).to_owned()
    } else {
        String::new()
    };

    let mut index = head;
    while index < file.len() && file.kind(index) != TokenKind::LBrace {
        if file.kind(index) == TokenKind::Eof {
            return None;
        }
        if file.kind(index) == TokenKind::LParen {
            index = file.match_close(index)?;
        }
        index += 1;
    }
    let open = index;
    let close = file.match_close(open)?;
    let span = file.span_of(head, close);
    let fields = match kind {
        DeclarationKind::Enum => scan_variants(file, open, close),
        _ => scan_fields(file, open, close),
    };
    // A backed declaration's members are the bodies it provides; a family's are
    // the defaults a declaration that says nothing inherits. Both are worth
    // running, which is why both are scanned. A struct's `function` is not: it
    // is a method with a receiver the evaluator has no value for.
    let members = match kind {
        DeclarationKind::Form | DeclarationKind::Construct => scan_members(file, open, close),
        _ => Vec::new(),
    };
    let hooks = match kind {
        DeclarationKind::Construct => scan_hooks(file, open, close),
        _ => Vec::new(),
    };

    Some((
        Declaration {
            kind,
            name,
            family,
            fields,
            members,
            hooks,
            syntax: file.slice(span).to_owned(),
            span,
            source: Some(file.source),
            path: file.path.clone(),
            line: file.line_of(span.start),
            file_lines: file.line_count(),
            annotations,
        },
        close + 1,
    ))
}

/// Re-scans `text` as a standalone declaration.
///
/// This is what makes `Syntax` closed under the declaration-shaped operations:
/// `dropField` and `rewriteProperty` both return syntax that a further
/// `dropField` may be applied to, and both return *text*, so the way to answer
/// "is this still a declaration?" is to look at it again.
pub(crate) fn parse(text: &str) -> Option<Declaration> {
    let file = Lexed::new(SourceId::new(0), text);
    let mut index = 0usize;
    while index < file.len() && file.kind(index) == TokenKind::Eof {
        index += 1;
    }
    let (mut declaration, _) = scan(&file, 0)?;
    if declaration.kind == DeclarationKind::Other {
        return None;
    }
    // Detached text: the spans are offsets into `text`, not into any file, so
    // the file they came from is unknown rather than [`SourceId::new(0)`], which
    // is a real id belonging to a real file. See [`Declaration::source`].
    declaration.source = None;
    declaration.path = std::sync::Arc::from("");
    declaration.line = 0;
    declaration.file_lines = 0;
    for field in &mut declaration.fields {
        field.source = None;
    }
    Some(declaration)
}

/// Consumes a run of `@Name` / `@Derive(A, B)` annotations.
fn scan_annotations(file: &Lexed<'_>, start: usize) -> (Vec<Annotation>, usize) {
    let mut annotations = Vec::new();
    let mut index = start;
    while file.kind(index) == TokenKind::At {
        if !file.is_ident(index + 1) {
            break;
        }
        let mut name = file.text_at(index + 1).to_owned();
        let mut last = index + 1;
        // A qualified annotation — `@FFI.Extern` — keeps its dotted spelling so
        // it is never mistaken for a macro named after its first segment.
        while file.kind(last + 1) == TokenKind::Dot && file.is_ident(last + 2) {
            name.push('.');
            name.push_str(file.text_at(last + 2));
            last += 2;
        }
        let mut arguments = Vec::new();
        if file.kind(last + 1) == TokenKind::LParen {
            if let Some(close) = file.match_close(last + 1) {
                arguments = file
                    .split_group(last + 1, close)
                    .into_iter()
                    .map(|(first, end)| file.slice(file.span_of(first, end)).trim().to_owned())
                    .collect();
                last = close;
            }
        } else if file.kind(last + 1) == TokenKind::LBrace {
            // `@FFI.Extern { … }` and `@Export { … }` carry a block payload.
            if let Some(close) = file.match_close(last + 1) {
                last = close;
            }
        }
        annotations.push(Annotation {
            name,
            arguments,
            span: file.span_of(index, last),
        });
        index = last + 1;
    }
    (annotations, index)
}

/// Scans a construct family's `lifecycle { … }` section, if it has one.
///
/// A hook is `name() { … }`, optionally annotated. `lifecycle` is a contextual
/// identifier: a member of that name followed by a brace is the section, and
/// anything else called `lifecycle` is left alone.
fn scan_hooks(file: &Lexed<'_>, open: usize, close: usize) -> Vec<Hook> {
    let mut index = open + 1;
    while index < close {
        if file.is_word(index, "lifecycle") && file.kind(index + 1) == TokenKind::LBrace {
            let Some(section_close) = file.match_close(index + 1) else {
                return Vec::new();
            };
            return scan_hook_bodies(file, index + 1, section_close);
        }
        if matches!(
            file.kind(index),
            TokenKind::LBrace | TokenKind::LParen | TokenKind::LBracket
        ) {
            match file.match_close(index) {
                Some(end) => index = end + 1,
                None => return Vec::new(),
            }
            continue;
        }
        index += 1;
    }
    Vec::new()
}

/// Scans the hooks inside a `lifecycle { … }` section.
fn scan_hook_bodies(file: &Lexed<'_>, open: usize, close: usize) -> Vec<Hook> {
    let mut hooks = Vec::new();
    let mut index = open + 1;
    while index < close {
        let (annotations, after) = scan_annotations(file, index);
        if !file.is_ident(after) || file.kind(after + 1) != TokenKind::LParen {
            index = if after > index { after } else { index + 1 };
            continue;
        }
        let Some(arguments_close) = file.match_close(after + 1) else {
            break;
        };
        let brace = arguments_close + 1;
        if file.kind(brace) != TokenKind::LBrace {
            index = brace;
            continue;
        }
        let Some(end) = file.match_close(brace) else {
            break;
        };
        hooks.push(Hook {
            name: file.text_at(after).to_owned(),
            body: body_text(file, brace, end),
            comptime: annotations
                .iter()
                .any(|annotation| annotation.name == "Comptime"),
            span: file.span_of(index, end),
        });
        index = end + 1;
    }
    hooks
}

/// Scans the behaviour members of a declaration body.
///
/// Both spellings a construct-backed declaration may use: the `name { … }`
/// shorthand, and `function name(…) -> T { … }`. A `let` member is a field and
/// is scanned by [`scan_fields`] instead.
fn scan_members(file: &Lexed<'_>, open: usize, close: usize) -> Vec<Member> {
    let mut members = Vec::new();
    let mut index = open + 1;
    while index < close {
        // `function name(…) … { … }`. A `@Required` member has no body at all —
        // it states an obligation — so the search for one stops at the next
        // member rather than running on and swallowing that member's braces.
        if file.kind(index) == TokenKind::Function && file.is_ident(index + 1) {
            let name = file.text_at(index + 1).to_owned();
            let mut brace = index + 2;
            while brace < close && file.kind(brace) != TokenKind::LBrace {
                if matches!(
                    file.kind(brace),
                    TokenKind::Function | TokenKind::At | TokenKind::Let | TokenKind::Var
                ) {
                    break;
                }
                // A parameter list or a `[T]` is skipped whole: neither holds
                // the body, and a `(` here would otherwise be walked into.
                if matches!(file.kind(brace), TokenKind::LParen | TokenKind::LBracket) {
                    match file.match_close(brace) {
                        Some(end) => brace = end + 1,
                        None => break,
                    }
                    continue;
                }
                brace += 1;
            }
            if file.kind(brace) != TokenKind::LBrace {
                // A requirement, not an implementation. Nothing to run.
                index = brace.max(index + 1);
                continue;
            }
            let Some(end) = file.match_close(brace) else {
                break;
            };
            members.push(Member {
                name,
                body: body_text(file, brace, end),
            });
            index = end + 1;
            continue;
        }
        if matches!(
            file.kind(index),
            TokenKind::At | TokenKind::Let | TokenKind::Var
        ) {
            let (_, after) = scan_annotations(file, index);
            if matches!(file.kind(after), TokenKind::Let | TokenKind::Var)
                && file.is_ident(after + 1)
            {
                let end = member_end(file, after + 2, close);
                index = end.saturating_add(1);
                continue;
            }
        }
        // `name { … }`, the shorthand for the member the family calls `name`.
        if file.is_ident(index) && file.kind(index + 1) == TokenKind::LBrace {
            let name = file.text_at(index).to_owned();
            let Some(end) = file.match_close(index + 1) else {
                break;
            };
            members.push(Member {
                name,
                body: body_text(file, index + 1, end),
            });
            index = end + 1;
            continue;
        }
        index += 1;
    }
    members
}

/// The text between a body's braces.
fn body_text(file: &Lexed<'_>, open: usize, close: usize) -> String {
    file.slice(Span::from_bounds(
        file.span(open).end(),
        file.span(close).start,
    ))
    .to_owned()
}

/// Scans the `var` / `let` members of a declaration body.
fn scan_fields(file: &Lexed<'_>, open: usize, close: usize) -> Vec<Field> {
    let mut fields = Vec::new();
    let mut index = open + 1;
    while index < close {
        if file.kind(index) == TokenKind::Function {
            index = skip_member(file, index, close);
            continue;
        }
        if is_named_rule_start(file, index) {
            index = file
                .match_close(index + 1)
                .map_or(close, |end| end.saturating_add(1));
            continue;
        }
        if file.kind(index) != TokenKind::At
            && !matches!(file.kind(index), TokenKind::Let | TokenKind::Var)
        {
            index += 1;
            continue;
        }
        let (annotations, after) = scan_annotations(file, index);
        if !matches!(file.kind(after), TokenKind::Let | TokenKind::Var) || !file.is_ident(after + 1)
        {
            index = if after > index { after } else { index + 1 };
            continue;
        }
        let name = file.text_at(after + 1).to_owned();
        let end = member_end(file, after + 2, close);
        let (type_text, initializer) = split_annotation(file, after + 2, end);
        let span = file.span_of(index, end);
        fields.push(Field {
            name,
            type_text,
            initializer,
            syntax: file.slice(span).to_owned(),
            span,
            source: Some(file.source),
            annotations,
        });
        index = end + 1;
    }
    fields
}

/// Scans an enum body's variants.
///
/// A variant surfaces as a field: its name is the variant's, and its type is
/// the payload type or the empty string, which is what lets one derive macro
/// walk a struct and an enum with the same loop.
fn scan_variants(file: &Lexed<'_>, open: usize, close: usize) -> Vec<Field> {
    let mut fields = Vec::new();
    let mut index = open + 1;
    while index < close {
        if !file.is_ident(index) {
            index += 1;
            continue;
        }
        let name = file.text_at(index).to_owned();
        let mut last = index;
        let mut type_text = String::new();
        if file.kind(index + 1) == TokenKind::Colon {
            let end = payload_end(file, index + 2, close);
            type_text = file.slice(file.span_of(index + 2, end)).trim().to_owned();
            last = end;
        } else if file.kind(index + 1) == TokenKind::LParen {
            // `Rank(Int)` — the payload form with no default. Scanning only the
            // `Name: Type` form left the parenthesized payload unread AND the
            // scan positioned inside the parentheses, so the payload's TYPE was
            // then reflected as a variant of its own: `Note { Rank(Int) }` came
            // back as two variants, `Rank` and `Int`, and every derive built
            // over `target.fields` emitted an arm for a variant that does not
            // exist.
            if let Some(end) = file.match_close(index + 1) {
                if end > index + 2 {
                    type_text = file
                        .slice(file.span_of(index + 2, end - 1))
                        .trim()
                        .to_owned();
                }
                last = end;
            }
        }
        let span = file.span_of(index, last);
        fields.push(Field {
            name,
            type_text,
            initializer: String::new(),
            syntax: file.slice(span).to_owned(),
            span,
            source: Some(file.source),
            annotations: Vec::new(),
        });
        index = last + 1;
    }
    fields
}

/// The last token index of an enum variant's payload type, which starts at
/// `from`.
///
/// Variants have no leading keyword to end on, so the payload runs to the end
/// of its own line: `Ok: Int` on one line and `Error: AppError` on the next are
/// two variants, not one with a very long type.
fn payload_end(file: &Lexed<'_>, from: usize, close: usize) -> usize {
    let mut last = from;
    let mut index = from + 1;
    while index < close && !file.newline_before(index) && file.kind(index) != TokenKind::Comma {
        match file.kind(index) {
            TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => {
                match file.match_close(index) {
                    Some(end) => {
                        last = end;
                        index = end + 1;
                        continue;
                    }
                    None => break,
                }
            }
            _ => last = index,
        }
        index += 1;
    }
    last
}

/// The last token index of the member that starts at `from`.
///
/// A member ends where the next one begins: at the next annotation, `let`,
/// `var`, or `function` at the body's own depth, or at the closing brace.
fn member_end(file: &Lexed<'_>, from: usize, close: usize) -> usize {
    let mut index = from;
    let mut last = from;
    let mut saw_equals = false;
    let mut saw_value = false;
    while index < close {
        if is_named_rule_start(file, index) && (!saw_equals || saw_value) {
            break;
        }
        match file.kind(index) {
            TokenKind::At | TokenKind::Let | TokenKind::Var | TokenKind::Function => break,
            TokenKind::Equals => {
                saw_equals = true;
                last = index;
            }
            TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => {
                match file.match_close(index) {
                    Some(end) => {
                        last = end;
                        if saw_equals {
                            saw_value = true;
                        }
                        index = end + 1;
                        continue;
                    }
                    None => break,
                }
            }
            _ => {
                if saw_equals {
                    saw_value = true;
                }
                last = index;
            }
        }
        index += 1;
    }
    last.max(from.saturating_sub(1))
}

/// Splits `: Type = initializer` into its two written halves.
fn split_annotation(file: &Lexed<'_>, from: usize, end: usize) -> (String, String) {
    let mut index = from;
    if file.kind(index) == TokenKind::Colon {
        index += 1;
    }
    let type_start = index;
    let mut equals = None;
    while index <= end {
        match file.kind(index) {
            TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => {
                match file.match_close(index) {
                    Some(close) => index = close,
                    None => break,
                }
            }
            TokenKind::Equals => {
                equals = Some(index);
                break;
            }
            _ => {}
        }
        index += 1;
    }
    match equals {
        Some(at) if at > type_start => (
            file.slice(file.span_of(type_start, at - 1))
                .trim()
                .to_owned(),
            file.slice(file.span_of(at + 1, end)).trim().to_owned(),
        ),
        // `let x = 1`, with no written type: the `=` is the first token, so
        // there is no type half and the initializer starts *after* it. Slicing
        // from `from` here would hand back `= 1` and call it the initializer,
        // which is what a reader comparing against `1` would never match.
        Some(at) => (
            String::new(),
            file.slice(file.span_of(at + 1, end)).trim().to_owned(),
        ),
        None if end >= type_start => (
            file.slice(file.span_of(type_start, end)).trim().to_owned(),
            String::new(),
        ),
        None => (String::new(), String::new()),
    }
}

/// Skips a member with a `{ … }` body, returning the index just past it.
fn skip_member(file: &Lexed<'_>, from: usize, close: usize) -> usize {
    let mut index = from;
    while index < close {
        if file.kind(index) == TokenKind::LBrace {
            return file.match_close(index).map_or(close, |end| end + 1);
        }
        index += 1;
    }
    close
}

fn is_named_rule_start(file: &Lexed<'_>, index: usize) -> bool {
    file.is_ident(index) && file.kind(index + 1) == TokenKind::LBrace
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan_text(text: &str) -> Declaration {
        let file = Lexed::new(SourceId::new(0), text);
        scan(&file, 0).expect("a declaration").0
    }

    #[test]
    fn a_struct_reflects_its_fields() {
        let declaration = scan_text("struct Point {\n    var x: Int\n    var y: Int = 2\n}\n");
        assert_eq!(declaration.kind, DeclarationKind::Struct);
        assert_eq!(declaration.name, "Point");
        assert_eq!(declaration.fields.len(), 2);
        assert_eq!(declaration.fields[0].name, "x");
        assert_eq!(declaration.fields[0].type_text, "Int");
        assert_eq!(declaration.fields[0].initializer, "");
        assert_eq!(declaration.fields[1].initializer, "2");
    }

    #[test]
    fn an_enum_reflects_its_variants() {
        let declaration = scan_text("enum Color {\n    Red\n    Green\n    Blue\n}\n");
        assert_eq!(declaration.kind, DeclarationKind::Enum);
        let names: Vec<&str> = declaration
            .fields
            .iter()
            .map(|field| field.name.as_str())
            .collect();
        assert_eq!(names, vec!["Red", "Green", "Blue"]);
    }

    #[test]
    fn a_payload_variant_carries_its_type() {
        let declaration = scan_text("enum Outcome {\n    Ok: Int\n    Error: AppError\n}\n");
        assert_eq!(declaration.fields[0].type_text, "Int");
        assert_eq!(declaration.fields[1].type_text, "AppError");
    }

    #[test]
    fn a_parenthesized_payload_is_the_variants_type_not_a_variant() {
        // The `Name(Type)` form went unscanned, so the payload type came back as
        // a variant of its own and every derive emitted an arm for it.
        let declaration =
            scan_text("enum Note {\n    Blank\n    Rank(Int)\n    Tag(String)\n}\n");
        let names: Vec<&str> = declaration
            .fields
            .iter()
            .map(|field| field.name.as_str())
            .collect();
        assert_eq!(names, vec!["Blank", "Rank", "Tag"]);
        assert_eq!(declaration.fields[0].type_text, "");
        assert_eq!(declaration.fields[1].type_text, "Int");
        assert_eq!(declaration.fields[2].type_text, "String");
    }

    #[test]
    fn an_empty_payload_list_is_a_payload_less_variant() {
        let declaration = scan_text("enum Flag {\n    On()\n    Off\n}\n");
        assert_eq!(declaration.fields.len(), 2);
        assert_eq!(declaration.fields[0].name, "On");
        assert_eq!(declaration.fields[0].type_text, "");
    }

    #[test]
    fn a_field_annotation_is_preserved() {
        let declaration = scan_text(
            "MfxPanel Demo() {\n    @Tracked var count: Int = 7\n    let body: Int = 1\n}\n",
        );
        assert_eq!(declaration.kind, DeclarationKind::Form);
        assert_eq!(declaration.name, "Demo");
        assert_eq!(declaration.fields[0].annotations[0].name, "Tracked");
        assert_eq!(declaration.fields[0].initializer, "7");
        assert!(declaration.fields[0].syntax.starts_with("@Tracked"));
        assert!(declaration.fields[1].annotations.is_empty());
    }

    #[test]
    fn a_form_field_stops_before_a_named_rule_body() {
        let declaration = scan_text(
            "Widget DashboardShell() {\n                @State var status: String = \"ready\"\n\n                body {\n                    Text(status)\n                }\n            }",
        );

        assert_eq!(declaration.fields.len(), 1);
        assert_eq!(declaration.fields[0].name, "status");
        assert_eq!(declaration.fields[0].initializer, "\"ready\"");
        assert!(!declaration.fields[0].syntax.contains("body {"));
        assert_eq!(declaration.members.len(), 1);
        assert_eq!(declaration.members[0].name, "body");
        assert!(declaration.members[0].body.contains("Text(status)"));
    }

    #[test]
    fn a_form_field_keeps_a_braced_initializer_before_a_named_rule() {
        let declaration = scan_text(
            "Widget DashboardShell() {\n                @State var model: Model = Model { value: 1 }\n\n                body {\n                    Text(\"ready\")\n                }\n            }",
        );

        assert_eq!(declaration.fields.len(), 1);
        assert!(
            declaration.fields[0]
                .initializer
                .contains("Model { value: 1 }")
        );
        assert_eq!(declaration.members.len(), 1);
        assert_eq!(declaration.members[0].name, "body");
    }

    #[test]
    fn a_declaration_annotation_is_separate_from_its_syntax() {
        let declaration =
            scan_text("@Derive(Equatable, Clone)\nstruct Point {\n    var x: Int\n}\n");
        assert_eq!(declaration.annotations.len(), 1);
        assert_eq!(declaration.annotations[0].name, "Derive");
        assert_eq!(
            declaration.annotations[0].arguments,
            vec!["Equatable", "Clone"]
        );
        assert!(declaration.syntax.starts_with("struct Point"));
    }

    #[test]
    fn methods_are_not_fields() {
        let declaration = scan_text(
            "struct S {\n    var a: Int\n    function get() -> Int {\n        var hidden = 1\n        return hidden\n    }\n    var b: Int\n}\n",
        );
        let names: Vec<&str> = declaration
            .fields
            .iter()
            .map(|field| field.name.as_str())
            .collect();
        assert_eq!(names, vec!["a", "b"]);
    }

    #[test]
    fn an_untyped_field_reports_its_initializer_without_the_equals() {
        // `let enabled = true` has no written type, so the `=` is the first
        // token after the name — and the initializer is what follows it, not
        // the assignment itself.
        let declaration =
            scan_text("Lint Entry {\n    let enabled = true\n    let code = \"K1\"\n}\n");
        assert_eq!(declaration.fields[0].type_text, "");
        assert_eq!(declaration.fields[0].initializer, "true");
        assert_eq!(declaration.fields[1].initializer, "\"K1\"");
    }

    #[test]
    fn syntax_round_trips_through_a_reparse() {
        let text = "struct Point {\n    var x: Int = 1 // a comment\n}\n";
        let declaration = scan_text(text);
        let again = parse(&declaration.syntax).expect("a declaration");
        assert_eq!(again.name, "Point");
        assert_eq!(again.fields.len(), 1);
        assert!(again.syntax.contains("// a comment"));
    }
}
