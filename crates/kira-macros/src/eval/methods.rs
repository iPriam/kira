//! Expressions and the compile-time builtins a macro body calls.
//!
//! The reflection reads — the members of `Declaration`, `Statement`, and
//! `Field`, and the methods on the text-carrying types — live in
//! [`super::reflection`]; this half is what walks an expression tree and runs it.

use kira_diagnostics::Severity;
use kira_syntax_model::ast::{BinaryOp, CallArg, Expr, ExprId, UnaryOp};

use super::reflection::{as_text, member, method_on};
use super::{EvalError, Evaluator, Report};
use crate::diagnostics;
use crate::ksl;
use crate::value::{DeclarationValue, Value};

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
            // `Syntax` as well, for the same reason `Int` takes it below: half
            // of what a macro reflects on arrives as the text it was written
            // with, and comparing or joining that text is how a lint reasons
            // about it.
            ("String", [Value::Syntax(written)]) => Ok(Value::Str(written.text.clone())),
            // A configuration value that *is* a number — a threshold, a count —
            // reaches a macro as the text it was written with. Without this, a
            // macro could compare it to other text and nothing else.
            //
            // `Syntax` as well as `Str`, because a field's initializer is
            // syntax: that is the value such a number actually arrives as.
            ("Int", [Value::Int(n)]) => Ok(Value::Int(*n)),
            ("Int", [Value::Str(text)]) => parse_int(text),
            ("Int", [Value::Syntax(written)]) => parse_int(&written.text),
            // An identifier built from text, resolved at the use site: the
            // complement of writing one literally in the macro, which resolves
            // at the definition site. What lets generated code name a type or
            // call a conversion whose spelling only exists as a string — one
            // branch per spelling is the alternative, and it does not scale
            // past two.
            ("Identifier", [Value::Str(text)]) => identifier_from_text(text),
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
        let compiled = match super::compile(body) {
            Ok(compiled) => compiled,
            Err(super::BodyError::Lift {
                offset,
                message,
                further,
            }) => {
                let line = body[..offset.min(body.len())].matches('\n').count() + 1;
                let and_more = match further {
                    0 => String::new(),
                    _ => format!(" ({further} more follow it)"),
                };
                return Err(EvalError::coded(
                    diagnostics::UNCLOSED_QUOTE,
                    format!(
                        "the body of `{}`'s `{name}` has {message} at line {line}{and_more}",
                        declaration.name
                    ),
                ));
            }
            Err(super::BodyError::Parse) => {
                return Err(EvalError::coded(
                    diagnostics::EXPAND_SIGNATURE,
                    format!(
                        "the body of `{}`'s `{name}` does not parse",
                        declaration.name
                    ),
                ));
            }
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

/// Builds the use-site identifier `text` names, or refuses text no identifier
/// can spell.
///
/// The rule is the lexer's own: an underscore or ASCII letter first, then
/// underscores and ASCII alphanumerics, and never a keyword. A constructed
/// keyword would resolve as the keyword where it lands, which is a confusion
/// no diagnostic at the landing site could attribute — so it is refused here,
/// where the text is still visible.
fn identifier_from_text(text: &str) -> Result<Value, EvalError> {
    let mut bytes = text.bytes();
    let well_formed = match bytes.next() {
        Some(first) if first == b'_' || first.is_ascii_alphabetic() => {
            bytes.all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
        }
        _ => false,
    };
    if !well_formed {
        return Err(EvalError::coded(
            diagnostics::BAD_IDENTIFIER,
            format!(
                "`Identifier(\"{text}\")`, which is not an identifier: one starts with a letter or `_`, and holds letters, digits, and `_`"
            ),
        ));
    }
    if kira_syntax_model::TokenKind::keyword_from_text(text).is_some() {
        return Err(EvalError::coded(
            diagnostics::BAD_IDENTIFIER,
            format!("`Identifier(\"{text}\")`, which is a keyword: keywords cannot be identifiers"),
        ));
    }
    Ok(Value::Identifier(text.to_owned()))
}
