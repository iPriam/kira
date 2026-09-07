//! What a macro is handed when it reflects on a declaration.
//!
//! Every piece is a byte range of the original file rather than a node, for
//! the reason the module root gives: `Declaration.syntax` and `Field.syntax`
//! are the exact source text, and the span edits built on them must leave
//! what they do not touch byte-for-byte intact.

use kira_source::{FileSpan, SourceId, Span};

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
    /// A construct-backed declaration: `construct Name(params) extends Family
    /// { … }`.
    Form,
    /// `function name(…) { … }`
    Function,
    /// `distinct Name = Representation`
    Distinct,
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
            DeclarationKind::Distinct => "distinct",
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
            DeclarationKind::Distinct => "Distinct",
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
