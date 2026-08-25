//! The reflection surface: what a macro body can read off a value.
//!
//! Members of `Declaration`, `Statement`, and `Field`, the methods on the
//! text-carrying types, and the `Syntax` edits — everything answered from the
//! value alone. Running a declaration's member needs the evaluator, so it lives
//! with the expression walk rather than here.

use kira_runtime_abi::StringOp;

use super::{EvalError, FIELD_TYPE};
use crate::diagnostics;
use crate::syntax_ops::{self, SyntaxError};
use crate::value::{DeclarationValue, StatementValue, Value};

/// Reads a member of a reflection value.
pub(super) fn member(value: &Value, name: &str) -> Result<Value, EvalError> {
    match (value, name) {
        (Value::Declaration(declaration), "name") => {
            Ok(Value::Identifier(declaration.name.clone()))
        }
        (Value::Declaration(declaration), "fields") => Ok(Value::Array(
            declaration
                .fields
                .iter()
                .map(|field| Value::Field(Box::new(field.clone())))
                .collect(),
        )),
        // The family a construct-backed declaration is written in, as a
        // string, so a macro can select declarations by family without the
        // compiler knowing any family by name. Empty for every other form.
        (Value::Declaration(declaration), "family") => Ok(Value::Str(declaration.family.clone())),
        // The names of its behaviour members, in declaration order.
        (Value::Declaration(declaration), "memberNames") => Ok(Value::Array(
            declaration
                .members
                .iter()
                .map(|(name, _)| Value::Str(name.clone()))
                .collect(),
        )),
        // Which form the declaration wears, as the word `appliesTo` uses:
        // `struct`, `class`, `enum`, `construct`, `form`, `function`. The value
        // has carried this since reflection existed; without a way to read it, a
        // lint that cares about one kind of declaration had to guess from text.
        // Which form the declaration wears, as a case a macro body matches
        // rather than a string it compares. The set is closed and the compiler
        // owns it, so a `match` over it is checked and a misspelled arm is an
        // error instead of a branch that silently never runs.
        (Value::Declaration(declaration), "kind") => {
            Ok(Value::EnumCase(Box::new(crate::value::EnumCaseValue {
                enum_name: "DeclarationForm".to_owned(),
                variant: declaration.kind.to_owned(),
                payload: None,
            })))
        }
        // Where it sits, and how big the file holding it is.
        //
        // A macro is handed declarations, never files, so without these a lint
        // can say everything about what a file contains and nothing about how
        // long it is. Both are `0` for a declaration re-scanned from detached
        // text, which belongs to no file — a lint reading them must treat `0` as
        // "not in a file" rather than as a small number.
        (Value::Declaration(declaration), "line") => Ok(Value::Int(i64::from(declaration.line))),
        (Value::Declaration(declaration), "fileLines") => {
            Ok(Value::Int(i64::from(declaration.file_lines)))
        }
        // Which file, as an opaque number: two declarations share a file when
        // their `fileId` matches, and `-1` means detached text belonging to no
        // file. Opaque rather than a path because that is the whole question a
        // lint asks — a message that wants to *name* the file anchors at `at:`,
        // and the renderer resolves the path from the span.
        // Where the file came from, as written in the program's module list.
        // `""` when the caller that scanned it did not locate it, so a lint
        // matching on a path fragment must treat empty as "unplaceable" rather
        // than as a path that happens to match nothing.
        (Value::Declaration(declaration), "path") => {
            Ok(Value::Str(declaration.path.as_ref().to_owned()))
        }
        (Value::Declaration(declaration), "fileId") => Ok(Value::Int(
            declaration
                .span
                .map_or(-1, |span| i64::from(span.source.value())),
        )),
        // `target.syntax` is the declaration as written, so it points at the
        // declaration — this is what `Diagnostics.error(…, at: target.syntax)`
        // rides on.
        (Value::Declaration(declaration), "syntax") => {
            Ok(Value::read(declaration.syntax.clone(), declaration.span))
        }
        // The statements the declaration's body holds, parsed on demand.
        //
        // On demand because most declarations are not asked: a derive walking
        // fields never needs a body, and parsing every declaration in a program
        // to answer the few that do would be paid by every macro.
        (Value::Declaration(declaration), "body") => Ok(Value::Array(
            crate::body::statements_of(&declaration.syntax, declaration.span)
                .iter()
                .map(|statement| Value::Statement(Box::new(StatementValue::of(statement))))
                .collect(),
        )),
        (Value::Statement(statement), "kind") => Ok(Value::Str(statement.kind.to_owned())),
        (Value::Statement(statement), "syntax") => {
            Ok(Value::read(statement.syntax.clone(), statement.span))
        }
        // What an `if` or `while` tests, what a `for` walks, what a `match`
        // selects on. Empty for a statement that branches on nothing.
        (Value::Statement(statement), "head") => {
            Ok(Value::read(statement.head.clone(), statement.span))
        }
        (Value::Statement(statement), "body") => Ok(Value::Array(
            statement
                .body
                .iter()
                .map(|inner| Value::Statement(Box::new(inner.clone())))
                .collect(),
        )),
        (Value::Field(field), "name") => Ok(Value::Identifier(field.name.clone())),
        (Value::Field(field), FIELD_TYPE) => Ok(Value::TypeRef(field.type_text.clone())),
        // The initializer has no span of its own — the scan records where the
        // whole field sits, not where its `=` half starts. Pointing at the
        // field it belongs to is coarse but lands on the right code, which is
        // what a reader needs; pointing nowhere would not.
        (Value::Field(field), "initializer") => {
            Ok(Value::read(field.initializer.clone(), field.span))
        }
        (Value::Field(field), "syntax") => Ok(Value::read(field.syntax.clone(), field.span)),
        (Value::Array(items), "count") => Ok(Value::Int(items.len() as i64)),
        (Value::Str(text), "count") => Ok(Value::Int(text.chars().count() as i64)),
        (Value::Record(record), name) => record
            .members
            .iter()
            .find(|(member, _)| member == name)
            .map(|(_, value)| value.clone())
            .ok_or_else(|| {
                EvalError::unsupported(format!(
                    "reading `{name}` on a `{}`, which has no such member",
                    record.name
                ))
            }),
        (other, name) => Err(EvalError::unsupported(format!(
            "reading `{name}` on a `{}`",
            other.type_name()
        ))),
    }
}

/// Calls a method on a reflection value.
pub(super) fn method_on(value: &Value, method: &str, args: &[Value]) -> Result<Value, EvalError> {
    match (value, method, args) {
        // Whether the declaration writes a body for `name` itself. One that does
        // not inherits its family's default, which is a *different* declaration
        // — so a collector that wants the inherited answer asks this first and
        // falls back to the family.
        (Value::Declaration(declaration), "hasMember", [Value::Str(name)]) => Ok(Value::Bool(
            declaration.members.iter().any(|(member, _)| member == name),
        )),
        (Value::Identifier(name), "asString", []) => Ok(Value::Str(name.clone())),
        (Value::TypeRef(written), "asSyntax", []) => Ok(Value::built(written.clone())),
        (Value::Str(text), "asString", []) => Ok(Value::Str(text.clone())),
        // The same string surface a program has, so a macro body and the code
        // it generates agree on what text can do. Reached on every value that
        // carries text — a `Syntax` is exactly as searchable as a `String`,
        // which is what lets a lint look at a declaration it was handed.
        (receiver, method, args) if StringOp::from_method_name(method).is_some() => {
            let Some(text) = as_text(receiver) else {
                return Err(EvalError::unsupported(format!(
                    "`{method}` on a `{}`, which carries no text",
                    receiver.type_name()
                )));
            };
            let text = text.to_owned();
            let op = StringOp::from_method_name(method).unwrap_or(StringOp::Contains);
            if args.len() != op.argument_count() {
                return Err(EvalError::unsupported(format!(
                    "`{method}` with {} argument(s) rather than {}",
                    args.len(),
                    op.argument_count()
                )));
            }
            let mut written = Vec::with_capacity(args.len());
            for argument in args {
                written.push(text_of(argument)?);
            }
            Ok(string_operation(op, &text, &written))
        }
        (Value::Array(items), "count" | "len", []) => Ok(Value::Int(items.len() as i64)),
        // The span from this statement's start through `other`'s end.
        //
        // A fix often replaces a *run* of statements — `var i = 0` and the
        // `while` beneath it become one `for` — and a lint that could only name
        // them one at a time could describe the problem but never write the
        // repair. Two statements of the same body are all this joins; anything
        // else has no single span to give.
        (Value::Statement(statement), "through", [Value::Statement(other)]) => {
            let from = statement.local;
            let to = other.local;
            if to.end() < from.start {
                return Ok(Value::read(statement.syntax.clone(), statement.span));
            }
            let run = statement
                .text
                .get(from.start as usize..to.end() as usize)
                .unwrap_or(&statement.syntax)
                .to_owned();
            let joined = statement.span.map(|at| {
                kira_source::FileSpan::new(
                    at.source,
                    kira_source::Span::new(at.span.start, to.end() - from.start),
                )
            });
            Ok(Value::read(run, joined))
        }
        (Value::Field(field), "hasAnnotation", [Value::Str(name)]) => {
            Ok(Value::Bool(field.has_annotation(name)))
        }
        (Value::Syntax(syntax), "identifiers", []) => Ok(Value::Array(
            syntax_ops::identifiers(&syntax.text)
                .into_iter()
                .map(Value::Identifier)
                .collect(),
        )),
        // The three edits below all return syntax that no longer matches the
        // bytes it came from, so the result points nowhere: a span into the
        // original text would underline the wrong run after an edit shifted
        // everything past it.
        // `addMember` — how a macro gives a declaration a section it did not
        // write, a `lifecycle { … }` above all.
        (Value::Syntax(syntax), "addMember", [member]) => {
            let member = text_of(member)?;
            syntax_ops::add_member(&syntax.text, &member)
                .map(Value::built)
                .map_err(syntax_error)
        }
        (Value::Syntax(syntax), "dropField", [name]) => {
            let field = text_of(name)?;
            syntax_ops::drop_field(&syntax.text, &field)
                .map(Value::built)
                .map_err(syntax_error)
        }
        (Value::Syntax(syntax), "replaceIdentifier", [from, to]) => {
            let from = text_of(from)?;
            let to = text_of(to)?;
            Ok(Value::built(crate::rename::every(
                &syntax.text,
                &std::iter::once((from, to)).collect(),
            )))
        }
        (Value::Syntax(syntax), "rewriteProperty", [name, read, write]) => {
            let name = text_of(name)?;
            let read = text_of(read)?;
            let write = text_of(write)?;
            syntax_ops::rewrite_property(&syntax.text, &name, &read, &write)
                .map(Value::built)
                .map_err(syntax_error)
        }
        (other, method, args) => Err(EvalError::unsupported(format!(
            "`{method}` on a `{}` with {} argument(s)",
            other.type_name(),
            args.len()
        ))),
    }
}

/// Performs one string operation at compile time.
///
/// The answers match `Vm::perform_string_op` case for case — an empty separator
/// leaves the text whole, `trim` and the case pair follow characters rather than
/// bytes — because a macro that reasons about text and a program that does must
/// not disagree about what the text says.
fn string_operation(op: StringOp, text: &str, arguments: &[String]) -> Value {
    match (op, arguments) {
        (StringOp::Contains, [needle]) => Value::Bool(text.contains(needle)),
        (StringOp::StartsWith, [prefix]) => Value::Bool(text.starts_with(prefix)),
        (StringOp::EndsWith, [suffix]) => Value::Bool(text.ends_with(suffix)),
        (StringOp::Replace, [from, to]) => Value::Str(text.replace(from, to)),
        (StringOp::Trim, []) => Value::Str(text.trim().to_owned()),
        (StringOp::Lowercase, []) => Value::Str(text.to_lowercase()),
        (StringOp::Uppercase, []) => Value::Str(text.to_uppercase()),
        (StringOp::Split, [separator]) => {
            let pieces: Vec<Value> = if separator.is_empty() {
                vec![Value::Str(text.to_owned())]
            } else {
                text.split(separator.as_str())
                    .map(|piece| Value::Str(piece.to_owned()))
                    .collect()
            };
            Value::Array(pieces)
        }
        // The arity was checked by the caller, so this is unreachable in
        // practice; answering `Void` beats a panic in a compiler.
        _ => Value::Void,
    }
}

/// The source text a `Syntax`, `Identifier`, `TypeRef`, or `String` carries.
fn text_of(value: &Value) -> Result<String, EvalError> {
    match value {
        Value::Syntax(syntax) => Ok(syntax.text.clone()),
        Value::Identifier(text) | Value::TypeRef(text) | Value::Str(text) => Ok(text.clone()),
        other => Err(EvalError::unsupported(format!(
            "a `{}` where syntax or a name is needed",
            other.type_name()
        ))),
    }
}

/// Turns a `Syntax` edit failure into its `KMAC` code.
fn syntax_error(error: SyntaxError) -> EvalError {
    match error {
        SyntaxError::NotADeclaration => EvalError::coded(
            diagnostics::NOT_A_DECLARATION,
            "this `Syntax` is not a declaration, so it has no fields or properties to edit",
        ),
        SyntaxError::NoSuchField(name) => EvalError::coded(
            diagnostics::NO_SUCH_FIELD,
            format!("this declaration has no field named `{name}`"),
        ),
        SyntaxError::AlreadyHasHook(name) => EvalError::coded(
            diagnostics::NO_SUCH_FIELD,
            format!(
                "this declaration writes a `{name}` lifecycle hook and the macro adds one: a \
                 hook a macro supplies is the runtime's half of the contract, so remove the \
                 hand-written one rather than declaring it twice"
            ),
        ),
        SyntaxError::WriteThroughProperty(name) => EvalError::coded(
            diagnostics::WRITE_THROUGH_WRAPPER,
            format!(
                "`{name}` is a wrapped property, so there is no place to write through: read the \
                 value, change the copy, and assign it back"
            ),
        ),
    }
}

/// The text a value carries, for the types that carry one.
pub(super) fn as_text(value: &Value) -> Option<&str> {
    match value {
        // Compared by text alone: two pieces of syntax reading the same are
        // equal however each one got here, which is what keeps `field.type ==
        // "Int"` working whether the type was read or built.
        Value::Syntax(syntax) => Some(&syntax.text),
        Value::Str(text) | Value::Identifier(text) | Value::TypeRef(text) => Some(text),
        _ => None,
    }
}

/// Builds the reflection value a macro's `Declaration` parameter is bound to.
pub(crate) fn declaration_value(declaration: &crate::decl::Declaration) -> Value {
    Value::Declaration(Box::new(DeclarationValue::of(declaration)))
}
