//! The values a `comptime macro` body computes with, and how each one splices.
//!
//! The compile-time surface is deliberately small: the scalars Kira already
//! has, arrays of them, and the four reflection types (`Syntax`, `Identifier`,
//! `TypeRef`, and the `Declaration` / `Field` pair). None of it is ever lowered
//! to a backend — a macro body runs here and its *output* is what a backend
//! sees.

use crate::decl;

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
    Syntax(String),
    /// An identifier obtained from reflection or from a quote.
    Identifier(String),
    /// A written type reference.
    TypeRef(String),
    /// A declaration a macro was applied to.
    Declaration(Box<DeclarationValue>),
    /// One field or enum variant of a declaration.
    Field(Box<FieldValue>),
    /// An array of values.
    Array(Vec<Value>),
}

/// A `Declaration` as the reflection API exposes it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DeclarationValue {
    /// The declaration's name.
    pub(crate) name: String,
    /// Its fields, or an enum's variants, in declaration order.
    pub(crate) fields: Vec<FieldValue>,
    /// Its exact source text.
    pub(crate) syntax: String,
    /// The `appliesTo` word for the form it wears.
    pub(crate) kind: &'static str,
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
    /// The names of the annotations written on it.
    pub(crate) annotations: Vec<String>,
}

impl DeclarationValue {
    /// Builds the reflection view of a scanned declaration.
    pub(crate) fn of(declaration: &decl::Declaration) -> Self {
        Self {
            name: declaration.name.clone(),
            fields: declaration.fields.iter().map(FieldValue::of).collect(),
            syntax: declaration.syntax.clone(),
            kind: declaration.kind.word(),
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
            annotations: field
                .annotations
                .iter()
                .map(|annotation| annotation.name.clone())
                .collect(),
        }
    }
}

impl Value {
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
            Value::Array(_) => "[T]",
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
            Value::Syntax(text) => Some(text.clone()),
            Value::Identifier(name) => Some(name.clone()),
            Value::TypeRef(written) => Some(written.clone()),
            Value::Str(text) => Some(quote_string(text)),
            Value::Int(value) => Some(value.to_string()),
            Value::Bool(value) => Some(value.to_string()),
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
            Value::Void | Value::Declaration(_) | Value::Field(_) => None,
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
        assert_eq!(
            Value::Syntax("a + b".to_owned()).splice().as_deref(),
            Some("a + b")
        );
    }

    #[test]
    fn an_array_splices_each_element() {
        let array = Value::Array(vec![
            Value::Syntax("one".to_owned()),
            Value::Syntax("two".to_owned()),
        ]);
        assert_eq!(array.splice().as_deref(), Some("one\ntwo"));
    }

    #[test]
    fn a_declaration_has_no_splice_rule() {
        let declaration = Value::Declaration(Box::new(DeclarationValue {
            name: "S".to_owned(),
            fields: Vec::new(),
            syntax: String::new(),
            kind: "struct",
        }));
        assert_eq!(declaration.splice(), None);
    }

    #[test]
    fn a_string_literal_escapes_its_quotes() {
        assert_eq!(quote_string("a\"b"), "\"a\\\"b\"");
    }
}
