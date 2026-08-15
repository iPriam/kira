//! Expressions, the reflection API's members, and the two compile-time
//! namespaces (`Syntax` and `Diagnostics`).
//!
//! Everything a macro body can *ask* is here. The split from the statement
//! walker is by size, not by principle: both halves are the same evaluator.

use kira_diagnostics::Severity;
use kira_runtime_abi::StringOp;
use kira_syntax_model::ast::{BinaryOp, CallArg, Expr, ExprId, UnaryOp};

use super::{EvalError, Evaluator, FIELD_TYPE, Report};
use crate::diagnostics;
use crate::ksl;
use crate::syntax_ops::{self, SyntaxError};
use crate::value::{DeclarationValue, StatementValue, Value};

impl Evaluator<'_> {
    /// The case `enum_name.variant` names, when `enum_name` is a declared enum.
    ///
    /// `Ok(None)` when the name is not an enum at all, so an ordinary field read
    /// carries on. A *known* enum with an unknown case is an error naming what
    /// the enum does have: that is the whole reason to write a case rather than
    /// a string.
    fn enum_case(&self, enum_name: &str, variant: &str) -> Result<Option<Value>, EvalError> {
        let Some(variants) = self.enums.get(enum_name) else {
            return Ok(None);
        };
        if !variants.iter().any(|declared| declared == variant) {
            let spellings: Vec<String> = variants.iter().map(|case| format!("`.{case}`")).collect();
            return Err(EvalError::unsupported(format!(
                "`{enum_name}.{variant}`, because `{enum_name}` has no case `{variant}` — it has {}",
                spellings.join(", ")
            )));
        }
        Ok(Some(Value::EnumCase(Box::new(
            crate::value::EnumCaseValue {
                enum_name: enum_name.to_owned(),
                variant: variant.to_owned(),
                payload: None,
            },
        ))))
    }

    /// Evaluates the expression at `id`.
    pub(super) fn value(&mut self, id: ExprId) -> Result<Value, EvalError> {
        match self.expr(id).clone() {
            Expr::Int { value, .. } => Ok(Value::Int(value)),
            Expr::Bool { value, .. } => Ok(Value::Bool(value)),
            Expr::Str { value, .. } => Ok(Value::Str(value)),
            Expr::Name { symbol, .. } => {
                let name = self.name(symbol).to_owned();
                self.lookup(&name).cloned().ok_or_else(|| {
                    EvalError::unsupported(format!("the name `{name}`, which is not bound here"))
                })
            }
            Expr::ArrayLit { elements, .. } => {
                let mut items = Vec::with_capacity(elements.len());
                for element in elements {
                    items.push(self.value(element)?);
                }
                Ok(Value::Array(items))
            }
            Expr::Unary { op, operand, .. } => {
                let value = self.value(operand)?;
                match (op, value) {
                    (UnaryOp::Neg, Value::Int(n)) => Ok(Value::Int(-n)),
                    (UnaryOp::Not, Value::Bool(b)) => Ok(Value::Bool(!b)),
                    (_, other) => Err(EvalError::unsupported(format!(
                        "that operator on a `{}`",
                        other.type_name()
                    ))),
                }
            }
            Expr::Binary { op, lhs, rhs, .. } => {
                let left = self.value(lhs)?;
                let right = self.value(rhs)?;
                binary(op, left, right)
            }
            Expr::Conditional {
                cond,
                then,
                otherwise,
                ..
            } => {
                if self.condition(cond)? {
                    self.value(then)
                } else {
                    self.value(otherwise)
                }
            }
            Expr::Index { base, index, .. } => {
                let array = self.value(base)?;
                let at = self.value(index)?;
                match (array, at) {
                    (Value::Array(items), Value::Int(at)) => items
                        .get(usize::try_from(at).unwrap_or(usize::MAX))
                        .cloned()
                        .ok_or_else(|| {
                            EvalError::unsupported(format!("an index of {at} outside the array"))
                        }),
                    (other, _) => Err(EvalError::unsupported(format!(
                        "indexing a `{}`",
                        other.type_name()
                    ))),
                }
            }
            Expr::Field { base, field, .. } => {
                let name = self.name(field).to_owned();
                // `Backend.Glsl` names a case of an enum the *program* declares,
                // so it resolves before the base is evaluated — there is no
                // value called `Backend`, and asking for one would report that
                // the name is unbound rather than that the case is misspelled.
                if let Expr::Name { symbol, .. } = self.expr(base).clone()
                    && let Some(case) = self.enum_case(self.name(symbol), &name)?
                {
                    return Ok(case);
                }
                let value = self.value(base)?;
                // `Self.test` inside a lifecycle hook: a name the reflection
                // surface does not answer, on a declaration that declares a
                // member of that name, is that member — run now. Which is what
                // makes a hook read like the code it replaces.
                if let Value::Declaration(declaration) = &value
                    && member(&value, &name).is_err()
                    && declaration.members.iter().any(|(each, _)| each == &name)
                {
                    let declaration = declaration.clone();
                    return self.member_value(&declaration, &name);
                }
                member(&value, &name)
            }
            // `.Variant` / `.Variant(payload)`. Which enum it belongs to is the
            // expected type's business everywhere else in the language, and an
            // `expand` body has no expected type here — so the case carries the
            // variant and, when the arm that reads it needs one, the payload.
            Expr::DotMember { name, args, .. } => {
                let variant = self.name(name).to_owned();
                let payload = match args {
                    Some(arguments) if !arguments.is_empty() => {
                        if arguments.len() > 1 {
                            return Err(EvalError::unsupported(format!(
                                "a variant carrying {} payloads; one is the most a variant holds",
                                arguments.len()
                            )));
                        }
                        Some(self.value(arguments[0])?)
                    }
                    _ => None,
                };
                Ok(Value::EnumCase(Box::new(crate::value::EnumCaseValue {
                    enum_name: String::new(),
                    variant,
                    payload,
                })))
            }
            Expr::Call {
                callee, args, span, ..
            } => {
                let name = self.name(callee).to_owned();
                let _ = span;
                self.call(&name, &args)
            }
            Expr::MethodCall {
                receiver,
                method,
                args,
                ..
            } => {
                let name = self.name(method).to_owned();
                self.method_call(receiver, &name, &args)
            }
            other => Err(EvalError::unsupported(format!(
                "a {} expression",
                super::shape(&other)
            ))),
        }
    }

    /// Evaluates a call's arguments, ignoring labels.
    fn arguments(&mut self, args: &[CallArg]) -> Result<Vec<Value>, EvalError> {
        let mut values = Vec::with_capacity(args.len());
        for argument in args {
            values.push(self.value(argument.value)?);
        }
        Ok(values)
    }

    /// A free call: a lifted `quote`, or a compile-time builtin.
    fn call(&mut self, callee: &str, args: &[CallArg]) -> Result<Value, EvalError> {
        let values = self.arguments(args)?;
        if let Some(template) = super::template_of(callee) {
            return self.render(template, &values);
        }
        match (callee, values.as_slice()) {
            ("String", [Value::Int(n)]) => Ok(Value::Str(n.to_string())),
            ("String", [Value::Bool(b)]) => Ok(Value::Str(b.to_string())),
            ("String", [Value::Str(text)]) => Ok(Value::Str(text.clone())),
            // A configuration value that *is* a number — a threshold, a count —
            // reaches a macro as the text it was written with. Without this, a
            // macro could compare it to other text and nothing else.
            //
            // `Syntax` as well as `Str`, because a field's initializer is
            // syntax: that is the value such a number actually arrives as.
            ("Int", [Value::Int(n)]) => Ok(Value::Int(*n)),
            ("Int", [Value::Str(text)]) => parse_int(text),
            ("Int", [Value::Syntax(written)]) => parse_int(&written.text),
            // A `comptime function` the program declared, which is how one
            // composes with another. Asked last, so a builtin of the same name
            // keeps its meaning.
            _ => match self.call_comptime(callee, &values) {
                Some(result) => result,
                None => Err(EvalError::unsupported(format!(
                    "a call to `{callee}` with {} argument(s)",
                    values.len()
                ))),
            },
        }
    }

    /// A method call, on a value or on a compile-time namespace.
    fn method_call(
        &mut self,
        receiver: ExprId,
        method: &str,
        args: &[CallArg],
    ) -> Result<Value, EvalError> {
        // `Syntax` and `Diagnostics` are namespaces rather than values: they are
        // recognized by name, and only when nothing of that name is bound —
        // a macro that binds `Syntax` gets its own binding.
        if let Expr::Name { symbol, .. } = self.expr(receiver).clone() {
            let name = self.name(symbol).to_owned();
            if self.lookup(&name).is_none() {
                let values = self.arguments(args)?;
                return self.namespace_call(&name, method, &values);
            }
            // `append` writes through its receiver, so the receiver has to be a
            // place rather than a value.
            if method == "append" {
                let values = self.arguments(args)?;
                let [item] = values.as_slice() else {
                    return Err(EvalError::unsupported(
                        "`append` with other than one argument",
                    ));
                };
                let Some(Value::Array(items)) = self.lookup(&name) else {
                    return Err(EvalError::unsupported(format!(
                        "`append` on `{name}`, which is not an array"
                    )));
                };
                let mut items = items.clone();
                items.push(item.clone());
                self.assign(&name, Value::Array(items))?;
                return Ok(Value::Void);
            }
        }
        let value = self.value(receiver)?;
        let values = self.arguments(args)?;
        // Running a declaration's member needs the evaluator, so it cannot live
        // in `method_on` with the pure reads.
        if let (Value::Declaration(declaration), "value") = (&value, method) {
            let [Value::Str(name)] = values.as_slice() else {
                return Err(EvalError::unsupported(
                    "`value` with other than one member name",
                ));
            };
            return self.member_value(declaration, name);
        }
        method_on(&value, method, &values)
    }

    /// Runs one member of a construct-backed declaration and hands back what it
    /// answered.
    ///
    /// This is what lets a family's declarations be read as **data during
    /// compilation** rather than as code a program runs at startup: a collector
    /// asking `declaration.value("path")` gets the string, not syntax that would
    /// fetch it later.
    ///
    /// The member's body runs on this same evaluator, so it may itself call a
    /// `comptime function` and use every statement form an `expand` body has.
    fn member_value(
        &mut self,
        declaration: &DeclarationValue,
        name: &str,
    ) -> Result<Value, EvalError> {
        let Some((_, body)) = declaration
            .members
            .iter()
            .find(|(member, _)| member == name)
        else {
            return Err(EvalError::unsupported(format!(
                "`{}` declares no member `{name}` to run",
                declaration.name
            )));
        };
        let Some(compiled) = super::compile(body) else {
            return Err(EvalError::coded(
                diagnostics::EXPAND_SIGNATURE,
                format!(
                    "the body of `{}`'s `{name}` does not parse",
                    declaration.name
                ),
            ));
        };
        let comptime = super::Comptime {
            functions: self.functions,
            shaders: self.shaders,
            platform: &self.platform.clone(),
            enums: &self.enums.clone(),
            testing: self.testing,
        };
        let (value, reported) = super::run_value(&compiled, Vec::new(), comptime, self.lint)?;
        self.reported.extend(reported);
        Ok(value)
    }

    /// A call on `Syntax` or `Diagnostics`.
    fn namespace_call(
        &mut self,
        namespace: &str,
        method: &str,
        values: &[Value],
    ) -> Result<Value, EvalError> {
        match (namespace, method, values) {
            ("Syntax", "join", [Value::Array(items), Value::Str(separator)]) => {
                let mut parts = Vec::with_capacity(items.len());
                for item in items {
                    parts.push(item.splice().ok_or_else(|| {
                        EvalError::coded(
                            diagnostics::NO_SPLICE_RULE,
                            format!("`Syntax.join` cannot join a `{}`", item.type_name()),
                        )
                    })?);
                }
                // Joined syntax is assembled, not read: the pieces may come from
                // anywhere, or from nowhere, so the join belongs to no one file.
                Ok(Value::built(parts.join(separator)))
            }
            // `Diagnostics.error("…", at: something)`, and its two quieter
            // siblings. The `at:` argument is whatever the macro is talking
            // *about*, and its span is where the caret goes. A macro that names
            // nothing, or names a value that came from no file, reports without
            // an anchor and the caller falls back to the macro's own
            // declaration.
            //
            // Severity is what separates a macro that refuses from a macro that
            // observes: `error` discards the expansion, `warning` and `note`
            // leave it in place. A lint is the second kind — it has an opinion
            // about code that is otherwise perfectly good.
            (
                "Diagnostics",
                method @ ("error" | "warning" | "note"),
                [Value::Str(message), rest @ ..],
            ) => {
                let severity = match method {
                    "warning" => Severity::Warning,
                    "note" => Severity::Note,
                    _ => Severity::Error,
                };
                // The remaining arguments are read by type rather than by
                // position, because the evaluator drops argument labels: the
                // value that came from somewhere is the anchor, and the string
                // is the code. That leaves `at:` and `code:` independently
                // omittable and order-insensitive, rather than one being
                // silently misread as the other when the pair is incomplete.
                let mut strings = rest.iter().filter_map(|value| match value {
                    Value::Str(text) => Some(text.clone()),
                    _ => None,
                });
                self.reported.push(Report {
                    severity,
                    message: message.clone(),
                    at: rest.iter().find_map(Value::anchor),
                    // `code:` then `fix:` — the evaluator drops labels, so the
                    // order is the contract.
                    code: strings.next(),
                    fix: strings.next(),
                });
                Ok(Value::Void)
            }
            (ksl::NAMESPACE, "compile", values) => ksl::compile(self.shaders, values),
            // What is being built, answered at compile time. A program that
            // asked a C library which platform it was on would be asking at run
            // time a question the compiler already answered.
            ("Build", "platform", []) => Ok(Value::Str(self.platform.clone())),
            // Whether `kira lint` asked for this run. False under every other
            // verb, which is what keeps a lint from running during `check`.
            ("Build", "linting", []) => Ok(Value::Bool(self.lint)),
            ("Build", "testing", []) => Ok(Value::Bool(self.testing)),
            _ => Err(EvalError::unsupported(format!(
                "`{namespace}.{method}` with {} argument(s)",
                values.len()
            ))),
        }
    }
}

/// Reads a member of a reflection value.
fn member(value: &Value, name: &str) -> Result<Value, EvalError> {
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
fn method_on(value: &Value, method: &str, args: &[Value]) -> Result<Value, EvalError> {
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

/// Applies a binary operator to two compile-time values.
fn binary(op: BinaryOp, left: Value, right: Value) -> Result<Value, EvalError> {
    use BinaryOp::{Add, And, Div, Eq, Ge, Gt, Le, Lt, Mul, Ne, Or, Rem, Sub};
    match (op, &left, &right) {
        (Add, Value::Int(a), Value::Int(b)) => Ok(Value::Int(a.wrapping_add(*b))),
        (Sub, Value::Int(a), Value::Int(b)) => Ok(Value::Int(a.wrapping_sub(*b))),
        (Mul, Value::Int(a), Value::Int(b)) => Ok(Value::Int(a.wrapping_mul(*b))),
        (Div, Value::Int(a), Value::Int(b)) if *b != 0 => Ok(Value::Int(a / b)),
        (Rem, Value::Int(a), Value::Int(b)) if *b != 0 => Ok(Value::Int(a % b)),
        (Add, Value::Str(a), Value::Str(b)) => Ok(Value::Str(format!("{a}{b}"))),
        (Lt, Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a < b)),
        (Le, Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a <= b)),
        (Gt, Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a > b)),
        (Ge, Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a >= b)),
        (And, Value::Bool(a), Value::Bool(b)) => Ok(Value::Bool(*a && *b)),
        (Or, Value::Bool(a), Value::Bool(b)) => Ok(Value::Bool(*a || *b)),
        (Eq, a, b) => Ok(Value::Bool(comparable(a, b)?)),
        (Ne, a, b) => Ok(Value::Bool(!comparable(a, b)?)),
        _ => Err(EvalError::unsupported(format!(
            "`{}` between a `{}` and a `{}`",
            op.spelling(),
            left.type_name(),
            right.type_name()
        ))),
    }
}

/// Equality over the scalar compile-time values.
///
/// Everything that carries text compares *by* its text, across the four types
/// that do. A macro that asks whether a field's type is `"Int"` is comparing a
/// `TypeRef` with a `String`, and answering "those are different types" would
/// be true and useless: the question it is really asking is whether the source
/// reads the same, and that has one answer.
fn comparable(left: &Value, right: &Value) -> Result<bool, EvalError> {
    match (left, right) {
        (Value::Int(a), Value::Int(b)) => Ok(a == b),
        (Value::Bool(a), Value::Bool(b)) => Ok(a == b),
        // Two cases are the same case when they hold the same variant and the
        // same payload. The enum's *name* is not part of it: a bare `.Enum`
        // written in a body never learned which enum it belongs to, and
        // demanding it match would make the one spelling a body can write never
        // equal to the one reflection hands back.
        (Value::EnumCase(a), Value::EnumCase(b)) => {
            if a.variant != b.variant {
                return Ok(false);
            }
            match (&a.payload, &b.payload) {
                (None, None) => Ok(true),
                (Some(a), Some(b)) => comparable(a, b),
                _ => Ok(false),
            }
        }
        (a, b) => match (as_text(a), as_text(b)) {
            (Some(a), Some(b)) => Ok(a == b),
            _ => Err(EvalError::unsupported(format!(
                "comparing a `{}` with a `{}`",
                a.type_name(),
                b.type_name()
            ))),
        },
    }
}

/// The text a value carries, for the types that carry one.
fn as_text(value: &Value) -> Option<&str> {
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

/// Reads `text` as a whole number, for `Int("700")`.
///
/// A refusal rather than a zero: a threshold written `"7O0"` that quietly became
/// 0 would turn a lint off, and silence is the one answer a reader cannot tell
/// from "nothing was found".
fn parse_int(text: &str) -> Result<Value, EvalError> {
    text.trim()
        .parse::<i64>()
        .map(Value::Int)
        .map_err(|_| EvalError::unsupported(format!("`Int(\"{text}\")`, which is not a number")))
}
