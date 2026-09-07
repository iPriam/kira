//! What one macro declaration says.
//!
//! The shapes the scanner fills in and every other module reads: which
//! invocation form a `comptime macro` wears, what a declarative macro's
//! parameters and template are, and where each declaration was written.

use kira_source::{SourceId, Span};

/// Which invocation form a `comptime macro` wears.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProceduralKind {
    /// `Name!(args)` in declaration, statement, or expression position.
    Function,
    /// `@Name` above a declaration.
    Attribute,
    /// `@Derive(Name, …)` above a declaration.
    Derive,
    /// `@Name` on a struct declares a wrapper template summoned by a field.
    Wrapper,
    /// Runs once for the whole program, over every declaration in it.
    ///
    /// The one kind that is not summoned by a site. Every other form is
    /// attached to the declaration or call it rewrites, so none of them can
    /// answer "which declarations does this program have?" — a suite runner
    /// needs exactly that, and it must be able to ask without the compiler
    /// knowing the family it is looking for.
    ///
    /// Its `expand` takes the declarations and returns the source of a file
    /// appended to the program, rather than an edit to an existing one: there
    /// is no site to splice into, and inventing one would make the answer
    /// depend on file order.
    Collector,
}

impl ProceduralKind {
    /// The `kind { … }` word this variant is written with.
    pub(crate) fn from_word(word: &str) -> Option<Self> {
        match word {
            "function" => Some(ProceduralKind::Function),
            "attribute" => Some(ProceduralKind::Attribute),
            "derive" => Some(ProceduralKind::Derive),
            "wrapper" => Some(ProceduralKind::Wrapper),
            "collector" => Some(ProceduralKind::Collector),
            _ => None,
        }
    }
}

/// What a declarative macro parameter captures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FragmentKind {
    /// A single expression, captured call-by-value and evaluated once.
    Expr,
    /// An assignable lvalue path, substituted where the template reads or
    /// writes it.
    Place,
}

/// One declarative macro parameter.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Fragment {
    /// The parameter's name, as the template refers to it.
    pub(crate) name: String,
    /// What the parameter captures.
    pub(crate) kind: FragmentKind,
}

/// A `macro Name(p: expr) { expand { … } }` declaration.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Declarative {
    /// The macro's name.
    pub(crate) name: String,
    /// Its fragment parameters, in order.
    pub(crate) fragments: Vec<Fragment>,
    /// The text between the braces of `expand { … }`.
    pub(crate) template: String,
    /// Where it was written, for a diagnostic about the declaration itself.
    pub(crate) source: SourceId,
    /// The span of its name.
    pub(crate) span: Span,
}

/// A `comptime function name(…) -> T { … }` declaration.
///
/// Ordinary Kira that runs during compilation. Its body goes on the same
/// evaluator a `comptime macro`'s `expand` runs on — the difference is only what
/// each hands back: a macro returns syntax to splice, and this returns a *value*
/// that becomes a literal at the call site.
///
/// Which is why it needs no `!`. A macro is called `name!(…)` because what
/// happens there is code substitution and the reader should see it; a comptime
/// function's call site is a value, indistinguishable from writing the answer
/// out, so it reads as the ordinary call it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ComptimeFunction {
    /// The function's name.
    pub(crate) name: String,
    /// Its parameter names, in order.
    pub(crate) parameters: Vec<String>,
    /// The text between the braces of its body.
    pub(crate) body: String,
    /// The span that text covers, so a failure inside the body points at the
    /// opener the author wrote rather than at the function's name.
    pub(crate) body_span: Span,
    /// Where it was written.
    pub(crate) source: SourceId,
    /// The span of its name.
    pub(crate) span: Span,
}

/// A `comptime macro Name { … }` declaration.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Procedural {
    /// The macro's name.
    pub(crate) name: String,
    /// Which invocation form it wears.
    pub(crate) kind: ProceduralKind,
    /// The declaration kinds it is legal on, for attribute/derive/wrapper.
    pub(crate) applies_to: Vec<String>,
    /// Whether a field annotation summons it over the enclosing declaration.
    pub(crate) trigger_field: bool,
    /// Whether its output replaces the annotated declaration.
    pub(crate) replace: bool,
    /// The `expand` parameter names, in order.
    pub(crate) parameters: Vec<String>,
    /// The text between the braces of `expand(…) -> Syntax { … }`.
    pub(crate) body: String,
    /// The span that text covers, so a failure inside the body points at the
    /// opener the author wrote rather than at the macro's name.
    pub(crate) body_span: Span,
    /// Where the declaration was written, for diagnostics about it.
    pub(crate) source: SourceId,
    /// The span of the declaration's name.
    pub(crate) span: Span,
}

pub(crate) fn kind_word(kind: ProceduralKind) -> &'static str {
    match kind {
        ProceduralKind::Function => "function",
        ProceduralKind::Attribute => "attribute",
        ProceduralKind::Derive => "derive",
        ProceduralKind::Wrapper => "wrapper",
        ProceduralKind::Collector => "collector",
    }
}
