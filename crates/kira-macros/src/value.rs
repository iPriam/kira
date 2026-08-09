//! The values a `comptime macro` body computes with, and how each one splices.
//!
//! The compile-time surface is deliberately small: the scalars Kira already
//! has, arrays of them, and the four reflection types (`Syntax`, `Identifier`,
//! `TypeRef`, and the `Declaration` / `Field` pair). None of it is ever lowered
//! to a backend — a macro body runs here and its *output* is what a backend
//! sees.

use kira_source::FileSpan;

use crate::decl;

/// A piece of Kira syntax, and where it was written.
///
/// The text is what splices; the span is what a diagnostic points at. They are
/// carried together because a macro that reports a problem with a declaration
/// has only the declaration's syntax in hand — without the span travelling
/// alongside, `Diagnostics.error(…, at: target.syntax)` has nothing to anchor to
/// and the caret lands on the macro instead of on what it was complaining
/// about.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SyntaxValue {
    /// The syntax as source text.
    pub(crate) text: String,
    /// Where the text was written, or `None` when it was built rather than
    /// read — a `quote`, a `Syntax.join`, anything assembled at compile time.
    pub(crate) span: Option<FileSpan>,
}

/// One compile-time value.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Value {
    /// The value of a statement, and of a call that returns nothing.
    Void,
    /// An integer.
    Int(i64),
    /// A boolean.
    Bool(bool),
    /// A string.
    Str(String),
    /// A piece of Kira syntax, as source text.
    Syntax(SyntaxValue),
    /// An identifier obtained from reflection or from a quote.
    Identifier(String),
    /// A written type reference.
    TypeRef(String),
    /// A declaration a macro was applied to.
    Declaration(Box<DeclarationValue>),
    /// One field or enum variant of a declaration.
    Field(Box<FieldValue>),
    /// One statement inside a declaration's body.
    Statement(Box<StatementValue>),
    /// A named bag of members a compile-time namespace handed back.
    Record(Box<RecordValue>),
    /// An array of values.
    Array(Vec<Value>),
    /// One case of an enum: the variant it holds, and its payload when it has
    /// one.
    ///
    /// A macro body writes a case the way the rest of the language does — a
    /// leading dot, `.Enum`, `.Some(4)` — and reads one back from reflection.
    /// The enum's *name* is carried when the value came from somewhere that
    /// knew it and is empty for a bare `.Variant`, because a dot literal
    /// resolves against the expected type and an `expand` body has no types to
    /// resolve against. Matching never needs it: an arm selects by variant.
    EnumCase(Box<EnumCaseValue>),
}

/// One case of an enum, as a macro body sees it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct EnumCaseValue {
    /// The enum this case belongs to, or `""` when it was written as a bare
    /// `.Variant` and nothing said which enum that is.
    pub(crate) enum_name: String,
    /// The variant's name.
    pub(crate) variant: String,
    /// The payload it carries, when the variant has one.
    pub(crate) payload: Option<Value>,
}

/// A record a compile-time namespace returns, read member by member.
///
/// Deliberately not a Kira value the macro can construct: it is how a namespace
/// answers with more than one thing at once. Its members are named by the
/// namespace that built it, and a macro body maps them onto whatever Kira type
/// it wants — which is what keeps the shape of `Ksl.compile`'s answer from
/// having to know the engine's artifact type.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RecordValue {
    /// What the record calls itself, for diagnostics.
    pub(crate) name: &'static str,
    /// Its members, in the order the namespace listed them.
    pub(crate) members: Vec<(String, Value)>,
}

/// A `Declaration` as the reflection API exposes it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DeclarationValue {
    /// The declaration's name.
    pub(crate) name: String,
    /// Its fields, or an enum's variants, in declaration order.
    pub(crate) fields: Vec<FieldValue>,
    /// Its behaviour members, as name and body text, in declaration order.
    ///
    /// Carried so a macro can *run* one during compilation rather than emit
    /// code that calls it at startup — see `Declaration.value(name)`.
    pub(crate) members: Vec<(String, String)>,
    /// Its exact source text.
    pub(crate) syntax: String,
    /// Where it was written, or `None` when it was re-scanned from detached
    /// text.
    pub(crate) span: Option<FileSpan>,
    /// The `DeclarationForm` variant a macro body matches this form as.
    pub(crate) kind: &'static str,
    /// The construct family backing it, or `""` when it is not a form.
    pub(crate) family: String,
    /// The path its file was read from, or `""` when that is not known.
    pub(crate) path: std::sync::Arc<str>,
    /// The 1-based line it starts on, or `0` when it was re-scanned.
    pub(crate) line: u32,
    /// How many lines its file holds, or `0` when it was re-scanned.
    pub(crate) file_lines: u32,
}

/// A `Statement` as the reflection API exposes it.
///
/// A thin view over [`crate::body::Statement`]: the same words, as values a
/// macro body compares and walks.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct StatementValue {
    /// Which statement form it is.
    pub(crate) kind: &'static str,
    /// Its exact source text.
    pub(crate) syntax: String,
    /// Where it was written, when that is a real place in a real file.
    pub(crate) span: Option<FileSpan>,
    /// The same span as an offset into [`StatementValue::text`].
    pub(crate) local: kira_source::Span,
    /// The declaration's whole source, so a rewrite can read the run it spans.
    pub(crate) text: std::sync::Arc<str>,
    /// The expression it branches on, or `""`.
    pub(crate) head: String,
    /// The statements directly inside it.
    pub(crate) body: Vec<StatementValue>,
}

impl StatementValue {
    /// Builds the reflection view of a read statement.
    pub(crate) fn of(statement: &crate::body::Statement) -> Self {
        Self {
            kind: statement.kind,
            syntax: statement.syntax.clone(),
            span: statement.span,
            local: statement.local,
            text: std::sync::Arc::clone(&statement.text),
            head: statement.head.clone(),
            body: statement.body.iter().map(StatementValue::of).collect(),
        }
    }
}

/// A `Field` as the reflection API exposes it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct FieldValue {
    /// The field's name, or the variant's.
    pub(crate) name: String,
    /// The written type, or `""` for a payload-less variant.
    pub(crate) type_text: String,
    /// The initial-value expression as written, or `""` when absent.
    pub(crate) initializer: String,
    /// The whole field declaration, annotations included.
    pub(crate) syntax: String,
    /// Where it was written, or `None` when it was re-scanned from detached
    /// text.
    pub(crate) span: Option<FileSpan>,
    /// The names of the annotations written on it.
    pub(crate) annotations: Vec<String>,
}

impl DeclarationValue {
    /// Builds the reflection view of a scanned declaration.
    pub(crate) fn of(declaration: &decl::Declaration) -> Self {
        Self {
            name: declaration.name.clone(),
            fields: declaration.fields.iter().map(FieldValue::of).collect(),
            members: declaration
                .members
                .iter()
                .map(|member| (member.name.clone(), member.body.clone()))
                .collect(),
            syntax: declaration.syntax.clone(),
            span: declaration.at(),
            kind: declaration.kind.variant(),
            family: declaration.family.clone(),
            path: declaration.path.clone(),
            line: declaration.line,
            file_lines: declaration.file_lines,
        }
    }
}

impl FieldValue {
    /// Whether an annotation named `name` was written on the field.
    pub(crate) fn has_annotation(&self, name: &str) -> bool {
        self.annotations.iter().any(|written| written == name)
    }

    /// Builds the reflection view of a scanned field.
    pub(crate) fn of(field: &decl::Field) -> Self {
        Self {
            name: field.name.clone(),
            type_text: field.type_text.clone(),
            initializer: field.initializer.clone(),
            syntax: field.syntax.clone(),
            span: field.at(),
            annotations: field
                .annotations
                .iter()
                .map(|annotation| annotation.name.clone())
                .collect(),
        }
    }
}

impl Value {
    /// Syntax that was built rather than read, so it points nowhere.
    pub(crate) fn built(text: impl Into<String>) -> Self {
        Value::Syntax(SyntaxValue {
            text: text.into(),
            span: None,
        })
    }

    /// Syntax read from `span`, which is where a diagnostic about it points.
    pub(crate) fn read(text: impl Into<String>, span: Option<FileSpan>) -> Self {
        Value::Syntax(SyntaxValue {
            text: text.into(),
            span,
        })
    }

    /// Where a diagnostic naming this value should point, when it knows.
    ///
    /// Only the three reflection values answer: they are the ones that came
    /// from somewhere. A string or an integer a macro computed has no place in
    /// any file, and guessing one would be worse than pointing nowhere.
    pub(crate) fn anchor(&self) -> Option<FileSpan> {
        match self {
            Value::Syntax(syntax) => syntax.span,
            Value::Declaration(declaration) => declaration.span,
            Value::Field(field) => field.span,
            Value::Statement(statement) => statement.span,
            _ => None,
        }
    }

    /// The type's user-facing name, for diagnostics.
    pub(crate) fn type_name(&self) -> &'static str {
        match self {
            Value::Void => "Void",
            Value::Int(_) => "Int",
            Value::Bool(_) => "Bool",
            Value::Str(_) => "String",
            Value::Syntax(_) => "Syntax",
            Value::Identifier(_) => "Identifier",
            Value::TypeRef(_) => "TypeRef",
            Value::Declaration(_) => "Declaration",
            Value::Field(_) => "Field",
            Value::Statement(_) => "Statement",
            Value::Record(record) => record.name,
            Value::Array(_) => "[T]",
            Value::EnumCase(_) => "enum case",
        }
    }

    /// How this value splices into a `quote`, or `None` when it has no splice
    /// rule.
    ///
    /// The rule is chosen by the value's type, never by where the splice sits,
    /// so `target.name` always splices as a bare name and
    /// `target.name.asString()` always splices as a quoted literal.
    pub(crate) fn splice(&self) -> Option<String> {
        match self {
            Value::Syntax(syntax) => Some(syntax.text.clone()),
            Value::Identifier(name) => Some(name.clone()),
            Value::TypeRef(written) => Some(written.clone()),
            Value::Str(text) => Some(quote_string(text)),
            Value::Int(value) => Some(value.to_string()),
            Value::Bool(value) => Some(value.to_string()),
            // A case splices as the language writes one: leading dot, and the
            // payload in parentheses when it carries one. The enum's name is
            // deliberately not spelled even when it is known, because the
            // position it lands in is what resolves a dot member — which is the
            // rule everywhere else a variant is written.
            Value::EnumCase(case) => match &case.payload {
                Some(payload) => Some(format!(".{}({})", case.variant, payload.splice()?)),
                None => Some(format!(".{}", case.variant)),
            },
            Value::Array(items) => {
                let mut parts = Vec::with_capacity(items.len());
                for item in items {
                    parts.push(item.splice()?);
                }
                // Elements are laid out one per line. The documented rule is
                // "nothing between them", and a newline is nothing — but it is
                // the nothing that cannot glue the last token of one element to
                // the first of the next. A comma-separated list is built with
                // `Syntax.join` instead, which is why nothing here needs a
                // separator.
                Some(parts.join("\n"))
            }
            // A record has no splice rule for the same reason a `Declaration`
            // has none: there is no one Kira form it obviously becomes. Its
            // members splice; it does not.
            Value::Void
            | Value::Declaration(_)
            | Value::Field(_)
            | Value::Statement(_)
            | Value::Record(_) => None,
        }
    }

    /// The value's truth, for a condition that must be a `Bool`.
    pub(crate) fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(value) => Some(*value),
            _ => None,
        }
    }
}

/// Renders `text` as a Kira string literal.
pub(crate) fn quote_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use kira_source::SourceId;

    #[test]
    fn each_type_splices_by_its_own_rule() {
        assert_eq!(
            Value::Identifier("Player".to_owned()).splice().as_deref(),
            Some("Player")
        );
        assert_eq!(
            Value::Str("Player".to_owned()).splice().as_deref(),
            Some("\"Player\"")
        );
        assert_eq!(Value::Int(-4).splice().as_deref(), Some("-4"));
        assert_eq!(Value::Bool(true).splice().as_deref(), Some("true"));
        assert_eq!(Value::built("a + b").splice().as_deref(), Some("a + b"));
    }

    #[test]
    fn an_array_splices_each_element() {
        let array = Value::Array(vec![Value::built("one"), Value::built("two")]);
        assert_eq!(array.splice().as_deref(), Some("one\ntwo"));
    }

    #[test]
    fn a_declaration_has_no_splice_rule() {
        let declaration = Value::Declaration(Box::new(DeclarationValue {
            members: Vec::new(),
            name: "S".to_owned(),
            fields: Vec::new(),
            syntax: String::new(),
            span: None,
            kind: "struct",
            family: String::new(),
            path: std::sync::Arc::from(""),
            line: 0,
            file_lines: 0,
        }));
        assert_eq!(declaration.splice(), None);
    }

    #[test]
    fn only_a_value_that_came_from_somewhere_anchors_a_diagnostic() {
        let at = FileSpan::new(SourceId::new(3), kira_source::Span::new(10, 4));
        assert_eq!(Value::read("Point", Some(at)).anchor(), Some(at));
        // Built syntax points nowhere: a `quote` was never written in a file.
        assert_eq!(Value::built("Point").anchor(), None);
        // Nor does a value a macro merely computed.
        assert_eq!(Value::Str("Point".to_owned()).anchor(), None);
        assert_eq!(Value::Int(1).anchor(), None);
    }

    #[test]
    fn a_record_has_no_splice_rule() {
        let record = Value::Record(Box::new(RecordValue {
            name: "KslCompiled",
            members: vec![("vertexEntry".to_owned(), Value::Str("v".to_owned()))],
        }));
        assert_eq!(record.splice(), None);
        assert_eq!(record.type_name(), "KslCompiled");
    }

    #[test]
    fn a_string_literal_escapes_its_quotes() {
        assert_eq!(quote_string("a\"b"), "\"a\\\"b\"");
    }
}
