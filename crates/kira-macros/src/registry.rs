//! Finding every `macro` and `comptime macro` declaration in a program, and
//! the model of what one says.
//!
//! Declarations are located at brace depth 0 of each file: `macro` and
//! `comptime` are contextual identifiers, so a local variable called `macro`
//! inside a function body is never mistaken for one. A macro is *visible*
//! program-wide rather than per file — the only top-level name a macro
//! introduces is its own, and both call sites and `@Derive` targets are
//! resolved against one table.
//!
//! Finding them is nonetheless a per-file question: [`collect_file`] reads one
//! file and nothing else, and [`Registry::absorb`] merges the results in file
//! order. Splitting it that way is what lets the frontend memoize the scan of a
//! dependency that has not changed instead of redoing it every compilation.

use std::collections::HashMap;

use kira_source::{SourceId, Span};
use kira_syntax_model::TokenKind;

use crate::diagnostics::{self, Reporter};
use crate::tokens::Lexed;

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
    fn from_word(word: &str) -> Option<Self> {
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
    /// Where the declaration was written, for diagnostics about it.
    pub(crate) source: SourceId,
    /// The span of the declaration's name.
    pub(crate) span: Span,
}

/// Every macro a program declares.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct Registry {
    declarative: HashMap<String, Declarative>,
    procedural: HashMap<String, Procedural>,
    comptime_functions: HashMap<String, ComptimeFunction>,
    enums: HashMap<String, Vec<String>>,
}

impl Registry {
    /// Adds one file's declarations, a later file's name winning over an
    /// earlier one's.
    ///
    /// The same rule the whole-program scan followed when it inserted into one
    /// map as it walked the files in order — merging per-file results in that
    /// same order reproduces it exactly.
    pub(crate) fn absorb(&mut self, file: &FileRegistry) {
        for declared in &file.declarative {
            self.declarative
                .insert(declared.name.clone(), declared.clone());
        }
        for declared in &file.procedural {
            self.procedural
                .insert(declared.name.clone(), declared.clone());
        }
        for declared in &file.comptime_functions {
            self.comptime_functions
                .insert(declared.name.clone(), declared.clone());
        }
        for (name, variants) in &file.enums {
            self.enums.insert(name.clone(), variants.clone());
        }
    }

    /// Whether the program declares no macros at all.
    ///
    /// The whole expansion pass is skipped when this holds, which is what keeps
    /// a program that never mentions a macro byte-identical to its own source.
    pub(crate) fn is_empty(&self) -> bool {
        self.declarative.is_empty()
            && self.procedural.is_empty()
            && self.comptime_functions.is_empty()
    }

    /// Every enum the program declares, by name, with its case names.
    ///
    /// A macro body naming `Backend.Glsl` is asking about a type the *program*
    /// declares, not one the compiler knows, so the evaluator has to be told
    /// what the program said.
    pub(crate) fn enums(&self) -> &HashMap<String, Vec<String>> {
        &self.enums
    }

    /// Every `comptime function` the program declares, for the evaluator.
    pub(crate) fn comptime_functions(&self) -> &HashMap<String, ComptimeFunction> {
        &self.comptime_functions
    }

    /// The `comptime function` named `name`, if there is one.
    pub(crate) fn comptime_function(&self, name: &str) -> Option<&ComptimeFunction> {
        self.comptime_functions.get(name)
    }

    /// Every `comptime function` name the program declares, in name order.
    ///
    /// A call to one is found by name alone — it wears no `!` — so the finder
    /// needs the whole set before it can tell one from an ordinary call.
    pub(crate) fn comptime_function_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.comptime_functions.keys().cloned().collect();
        names.sort();
        names
    }

    /// The declarative macro named `name`, if there is one.
    pub(crate) fn declarative(&self, name: &str) -> Option<&Declarative> {
        self.declarative.get(name)
    }

    /// The procedural macro named `name`, if there is one.
    pub(crate) fn procedural(&self, name: &str) -> Option<&Procedural> {
        self.procedural.get(name)
    }

    /// Every procedural macro of one kind, in name order.
    ///
    /// Sorted rather than in hash order so a program with two collectors
    /// appends their files in an order that does not change between runs.
    pub(crate) fn of_kind(&self, kind: ProceduralKind) -> Vec<&Procedural> {
        let mut found: Vec<&Procedural> = self
            .procedural
            .values()
            .filter(|declared| declared.kind == kind)
            .collect();
        found.sort_by(|a, b| a.name.cmp(&b.name));
        found
    }
}

/// Every macro **one file** declares, in declaration order.
///
/// Per file rather than per program because what a file declares depends on
/// nothing but that file's own bytes. That is what lets a caller memoize the
/// scan and pay for a dependency's macros once rather than once per
/// compilation; [`Registry::absorb`] puts the pieces back together in file
/// order.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct FileRegistry {
    /// The declarative macros this file declares, in declaration order.
    pub(crate) declarative: Vec<Declarative>,
    /// The procedural macros this file declares, in declaration order.
    pub(crate) procedural: Vec<Procedural>,
    /// The `comptime function`s this file declares, in declaration order.
    pub(crate) comptime_functions: Vec<ComptimeFunction>,
    /// Each `enum Name { … }` this file declares, with its case names.
    pub(crate) enums: Vec<(String, Vec<String>)>,
    /// The bytes each declaration covers, `macro` keyword through closing
    /// brace, so the caller can blank them.
    pub(crate) spans: Vec<Span>,
}

impl FileRegistry {
    /// Whether this file declares no macro at all.
    pub(crate) fn is_empty(&self) -> bool {
        self.declarative.is_empty()
            && self.procedural.is_empty()
            && self.comptime_functions.is_empty()
    }
}

/// Collects every macro declaration in one file, reporting malformed ones.
pub(crate) fn collect_file(file: &Lexed<'_>, reporter: &mut Reporter) -> FileRegistry {
    let mut found = FileRegistry::default();
    let mut index = 0usize;
    while index < file.len() {
        match file.kind(index) {
            TokenKind::Eof => break,
            TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => {
                match file.match_close(index) {
                    Some(end) => index = end + 1,
                    None => break,
                }
                continue;
            }
            _ => {}
        }
        // `enum Name { A B }` — read for its case names, so a macro body may
        // name one. Only the shape is taken; what the cases mean is the
        // program's business.
        if file.kind(index) == TokenKind::Enum
            && file.is_ident(index + 1)
            && file.kind(index + 2) == TokenKind::LBrace
            && let Some(end) = file.match_close(index + 2)
        {
            let name = file.text_at(index + 1).to_owned();
            let mut variants = Vec::new();
            let mut at = index + 3;
            while at < end {
                // A case is an identifier at the top of the body; anything
                // nested belongs to a payload and is skipped whole.
                match file.kind(at) {
                    TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => {
                        match file.match_close(at) {
                            Some(close) => at = close + 1,
                            None => break,
                        }
                        continue;
                    }
                    _ => {}
                }
                if file.is_ident(at) {
                    variants.push(file.text_at(at).to_owned());
                }
                at += 1;
            }
            found.enums.push((name, variants));
            index = end + 1;
            continue;
        }
        // `function` is a keyword token, not a contextual identifier the way
        // `macro` is, so it is matched by kind rather than by text.
        if file.is_word(index, "comptime") && file.kind(index + 1) == TokenKind::Function {
            match scan_comptime_function(file, index, reporter) {
                Some((function, span, next)) => {
                    found.spans.push(span);
                    found.comptime_functions.push(function);
                    index = next;
                    continue;
                }
                None => break,
            }
        }
        if file.is_word(index, "comptime") && file.is_word(index + 1, "macro") {
            match scan_procedural(file, index, reporter) {
                Some((macro_declaration, span, next)) => {
                    found.spans.push(span);
                    found.procedural.push(macro_declaration);
                    index = next;
                    continue;
                }
                None => break,
            }
        }
        if file.is_word(index, "macro")
            && file.is_ident(index + 1)
            && file.kind(index + 2) == TokenKind::LParen
        {
            match scan_declarative(file, index, reporter) {
                Some((macro_declaration, span, next)) => {
                    found.spans.push(span);
                    found.declarative.push(macro_declaration);
                    index = next;
                    continue;
                }
                None => break,
            }
        }
        index += 1;
    }
    found
}

/// Scans `macro Name(p: expr) { expand { … } }` starting at the `macro` word.
fn scan_declarative(
    file: &Lexed<'_>,
    start: usize,
    reporter: &mut Reporter,
) -> Option<(Declarative, Span, usize)> {
    let name = file.text_at(start + 1).to_owned();
    let open_params = start + 2;
    let close_params = file.match_close(open_params)?;
    let mut fragments = Vec::new();
    for (first, last) in file.split_group(open_params, close_params) {
        let parameter_name = file.text_at(first).to_owned();
        let kind = if file.kind(first + 1) == TokenKind::Colon {
            match file.text_at(first + 2) {
                "expr" => Some(FragmentKind::Expr),
                "place" => Some(FragmentKind::Place),
                _ => None,
            }
        } else {
            None
        };
        match kind {
            Some(kind) => fragments.push(Fragment {
                name: parameter_name,
                kind,
            }),
            None => reporter.error(
                file.source,
                file.span_of(first, last),
                diagnostics::FRAGMENT_KIND,
                format!(
                    "macro `{name}` parameter `{parameter_name}` must declare a fragment kind; \
                     v1 has `expr` and `place`"
                ),
            ),
        }
    }

    let open_body = close_params + 1;
    if file.kind(open_body) != TokenKind::LBrace {
        reporter.error(
            file.source,
            file.span(open_body),
            diagnostics::EXPAND_SIGNATURE,
            format!("macro `{name}` needs a body containing `expand {{ … }}`"),
        );
        return None;
    }
    let close_body = file.match_close(open_body)?;
    let template = match find_expand_block(file, open_body, close_body) {
        Some((open, close)) => file
            .slice(Span::from_bounds(
                file.span(open).end(),
                file.span(close).start,
            ))
            .to_owned(),
        None => {
            reporter.error(
                file.source,
                file.span_of(start, open_body),
                diagnostics::EXPAND_SIGNATURE,
                format!("macro `{name}` has no `expand {{ … }}` template"),
            );
            return None;
        }
    };

    Some((
        Declarative {
            name,
            fragments,
            template,
        },
        file.span_of(start, close_body),
        close_body + 1,
    ))
}

/// The `expand { … }` braces inside a declarative macro's body.
fn find_expand_block(file: &Lexed<'_>, open: usize, close: usize) -> Option<(usize, usize)> {
    let mut index = open + 1;
    while index < close {
        if file.is_word(index, "expand") && file.kind(index + 1) == TokenKind::LBrace {
            let block_close = file.match_close(index + 1)?;
            return Some((index + 1, block_close));
        }
        index += 1;
    }
    None
}

/// Scans `comptime function name(…) -> T { … }` starting at the `comptime` word.
///
/// The shape is an ordinary function declaration, so only the name, the
/// parameter names and the body text are taken: the written types are the
/// analyzer's business, and the evaluator binds arguments by position.
fn scan_comptime_function(
    file: &Lexed<'_>,
    start: usize,
    reporter: &mut Reporter,
) -> Option<(ComptimeFunction, Span, usize)> {
    let name_index = start + 2;
    if !file.is_ident(name_index) {
        reporter.error(
            file.source,
            file.span(name_index),
            diagnostics::BAD_KIND,
            "expected a name after `comptime function`",
        );
        return None;
    }
    let name = file.text_at(name_index).to_owned();
    let name_span = file.span(name_index);
    let open_parameters = name_index + 1;
    if file.kind(open_parameters) != TokenKind::LParen {
        reporter.error(
            file.source,
            name_span,
            diagnostics::EXPAND_SIGNATURE,
            format!("`comptime function {name}` needs a parameter list"),
        );
        return None;
    }
    let close_parameters = file.match_close(open_parameters)?;
    let parameters = file
        .split_group(open_parameters, close_parameters)
        .into_iter()
        .map(|(first, _)| file.text_at(first).to_owned())
        .collect();
    // Whatever sits between the parameters and the body is the written result
    // type, which the evaluator does not need: the value it produces carries its
    // own shape, and the analyzer checks the call site against the declaration.
    let mut open_body = close_parameters + 1;
    while open_body < file.len() && file.kind(open_body) != TokenKind::LBrace {
        if file.kind(open_body) == TokenKind::Eof {
            reporter.error(
                file.source,
                name_span,
                diagnostics::EXPAND_SIGNATURE,
                format!("`comptime function {name}` needs a `{{ … }}` body"),
            );
            return None;
        }
        open_body += 1;
    }
    let close_body = file.match_close(open_body)?;
    let body = file
        .slice(Span::from_bounds(
            file.span(open_body).end(),
            file.span(close_body).start,
        ))
        .to_owned();
    Some((
        ComptimeFunction {
            name,
            parameters,
            body,
            source: file.source,
            span: name_span,
        },
        file.span_of(start, close_body),
        close_body + 1,
    ))
}

/// Scans `comptime macro Name { … }` starting at the `comptime` word.
fn scan_procedural(
    file: &Lexed<'_>,
    start: usize,
    reporter: &mut Reporter,
) -> Option<(Procedural, Span, usize)> {
    let name_index = start + 2;
    if !file.is_ident(name_index) {
        reporter.error(
            file.source,
            file.span(name_index),
            diagnostics::BAD_KIND,
            "expected a name after `comptime macro`",
        );
        return None;
    }
    let name = file.text_at(name_index).to_owned();
    let open_body = name_index + 1;
    if file.kind(open_body) != TokenKind::LBrace {
        reporter.error(
            file.source,
            file.span(open_body),
            diagnostics::BAD_KIND,
            format!("`comptime macro {name}` needs a `{{ … }}` body"),
        );
        return None;
    }
    let close_body = file.match_close(open_body)?;

    let mut kind = None;
    let mut applies_to = Vec::new();
    let mut trigger_field = false;
    let mut replace = false;
    let mut parameters = Vec::new();
    let mut body = None;

    let mut index = open_body + 1;
    while index < close_body {
        if file.is_word(index, "expand") && file.kind(index + 1) == TokenKind::LParen {
            let close_parameters = file.match_close(index + 1)?;
            parameters = file
                .split_group(index + 1, close_parameters)
                .into_iter()
                .map(|(first, _)| file.text_at(first).to_owned())
                .collect();
            let mut brace = close_parameters + 1;
            while brace < close_body && file.kind(brace) != TokenKind::LBrace {
                brace += 1;
            }
            let close_expand = file.match_close(brace)?;
            body = Some(
                file.slice(Span::from_bounds(
                    file.span(brace).end(),
                    file.span(close_expand).start,
                ))
                .to_owned(),
            );
            index = close_expand + 1;
            continue;
        }
        if file.is_ident(index) && file.kind(index + 1) == TokenKind::LBrace {
            let member = file.text_at(index);
            let close_member = file.match_close(index + 1)?;
            // `function`, `struct`, `enum`, `class`, and `true` are all real
            // keywords, so a member's words are read by *text* rather than by
            // token kind: `kind { function }` names a macro form, not a
            // declaration.
            let words: Vec<&str> = (index + 2..close_member)
                .filter(|&word| file.kind(word) != TokenKind::Comma)
                .map(|word| file.text_at(word))
                .collect();
            match member {
                "kind" => {
                    kind = words
                        .first()
                        .and_then(|word| ProceduralKind::from_word(word))
                }
                "appliesTo" => {
                    applies_to = words.iter().map(|word| (*word).to_owned()).collect();
                }
                "trigger" => trigger_field = words.contains(&"field"),
                "replace" => replace = words.contains(&"true"),
                _ => {}
            }
            index = close_member + 1;
            continue;
        }
        index += 1;
    }

    let declaration_span = file.span_of(start, close_body);
    let name_span = file.span(name_index);
    let Some(kind) = kind else {
        reporter.error(
            file.source,
            name_span,
            diagnostics::BAD_KIND,
            format!(
                "`comptime macro {name}` needs `kind {{ function | attribute | derive | wrapper }}`"
            ),
        );
        return None;
    };
    let Some(body) = body else {
        reporter.error(
            file.source,
            name_span,
            diagnostics::EXPAND_SIGNATURE,
            format!("`comptime macro {name}` must define `expand`"),
        );
        return None;
    };

    let declared = Procedural {
        name,
        kind,
        applies_to,
        trigger_field,
        replace,
        parameters,
        body,
        source: file.source,
        span: name_span,
    };
    validate_shape(&declared, reporter);
    Some((declared, declaration_span, close_body + 1))
}

/// Checks the members of a `comptime macro` against each other.
fn validate_shape(declared: &Procedural, reporter: &mut Reporter) {
    let Procedural {
        name,
        kind,
        applies_to,
        trigger_field,
        replace,
        parameters,
        source,
        span,
        ..
    } = declared;
    let (source, span, kind) = (*source, *span, *kind);
    match kind {
        ProceduralKind::Collector if !applies_to.is_empty() => reporter.error(
            source,
            span,
            diagnostics::APPLIES_TO_PRESENCE,
            format!("`appliesTo` says which declarations a macro may annotate, so a `collector` macro like `{name}`, which annotates none, has none"),
        ),
        ProceduralKind::Function if !applies_to.is_empty() => reporter.error(
            source,
            span,
            diagnostics::APPLIES_TO_PRESENCE,
            format!("`appliesTo` says which declarations a macro may annotate, so a `function` macro like `{name}` has none"),
        ),
        ProceduralKind::Attribute | ProceduralKind::Derive | ProceduralKind::Wrapper
            if applies_to.is_empty() =>
        {
            reporter.error(
                source,
                span,
                diagnostics::APPLIES_TO_PRESENCE,
                format!("`{name}` annotates a declaration, so it must list the declaration kinds it is legal on with `appliesTo {{ … }}`"),
            );
        }
        _ => {}
    }
    if *trigger_field && !*replace {
        reporter.error(
            source,
            span,
            diagnostics::TRIGGER_WITHOUT_REPLACE,
            format!(
                "`{name}` is summoned by a field annotation, so it rewrites the declaration that \
                 carries the field: it must also declare `replace {{ true }}`"
            ),
        );
    }
    let expected = if kind == ProceduralKind::Wrapper {
        2
    } else {
        1
    };
    let parameter_count = parameters.len();
    if parameter_count != expected {
        reporter.error(
            source,
            span,
            diagnostics::EXPAND_SIGNATURE,
            format!(
                "`{name}` is a `{}` macro, so its `expand` takes {expected} parameter(s), not \
                 {parameter_count}",
                kind_word(kind)
            ),
        );
    }
}

/// The `kind { … }` word a [`ProceduralKind`] is written with.
pub(crate) fn kind_word(kind: ProceduralKind) -> &'static str {
    match kind {
        ProceduralKind::Function => "function",
        ProceduralKind::Attribute => "attribute",
        ProceduralKind::Derive => "derive",
        ProceduralKind::Wrapper => "wrapper",
        ProceduralKind::Collector => "collector",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect_text(text: &str) -> (Registry, Vec<kira_diagnostics::Diagnostic>) {
        let mut reporter = Reporter::new();
        let mut registry = Registry::default();
        registry.absorb(&collect_file(
            &Lexed::new(SourceId::new(0), text),
            &mut reporter,
        ));
        (registry, reporter.into_diagnostics())
    }

    #[test]
    fn a_declarative_macro_registers_its_fragments_and_template() {
        let (registry, diagnostics) = collect_text(
            "macro square(value: expr) {\n    expand {\n        value * value\n    }\n}\n",
        );
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        let declared = registry.declarative("square").expect("the macro");
        assert_eq!(declared.fragments.len(), 1);
        assert_eq!(declared.fragments[0].kind, FragmentKind::Expr);
        assert!(declared.template.contains("value * value"));
    }

    #[test]
    fn a_place_fragment_is_recognized() {
        let (registry, _) = collect_text("macro swap(a: place, b: place) { expand { a = b } }");
        let declared = registry.declarative("swap").expect("the macro");
        assert_eq!(declared.fragments.len(), 2);
        assert!(
            declared
                .fragments
                .iter()
                .all(|fragment| fragment.kind == FragmentKind::Place)
        );
    }

    #[test]
    fn a_procedural_macro_records_every_member() {
        let (registry, diagnostics) = collect_text(
            "comptime macro Tracked {\n    kind { attribute }\n    appliesTo { form }\n\
             \n    trigger { field }\n    replace { true }\n\
             \n    expand(target: Declaration) -> Syntax {\n        return quote { }\n    }\n}\n",
        );
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        let declared = registry.procedural("Tracked").expect("the macro");
        assert_eq!(declared.kind, ProceduralKind::Attribute);
        assert_eq!(declared.applies_to, vec!["form".to_owned()]);
        assert!(declared.trigger_field);
        assert!(declared.replace);
        assert_eq!(declared.parameters, vec!["target".to_owned()]);
    }

    #[test]
    fn a_field_trigger_without_replace_is_refused() {
        let (_, diagnostics) = collect_text(
            "comptime macro T {\n    kind { attribute }\n    appliesTo { form }\n\
             trigger { field }\n    expand(t: Declaration) -> Syntax { return quote { } }\n}\n",
        );
        assert!(
            diagnostics.iter().any(|d| d.has_code("KMAC029")),
            "{diagnostics:?}"
        );
    }

    #[test]
    fn a_missing_kind_is_refused() {
        let (registry, diagnostics) = collect_text(
            "comptime macro T {\n    expand(t: Declaration) -> Syntax { return quote { } }\n}\n",
        );
        assert!(registry.procedural("T").is_none());
        assert!(
            diagnostics.iter().any(|d| d.has_code("KMAC006")),
            "{diagnostics:?}"
        );
    }

    #[test]
    fn a_function_macro_may_not_declare_applies_to() {
        let (_, diagnostics) = collect_text(
            "comptime macro bits {\n    kind { function }\n    appliesTo { struct }\n\
             expand(input: Syntax) -> Syntax { return quote { } }\n}\n",
        );
        assert!(
            diagnostics.iter().any(|d| d.has_code("KMAC008")),
            "{diagnostics:?}"
        );
    }

    #[test]
    fn a_local_named_macro_is_not_a_declaration() {
        let (registry, diagnostics) =
            collect_text("function f() {\n    let macro = 1\n    return\n}\n");
        assert!(registry.is_empty());
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }
}
