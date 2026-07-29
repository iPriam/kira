//! Expressions, the reflection API's members, and the two compile-time
//! namespaces (`Syntax` and `Diagnostics`).
//!
//! Everything a macro body can *ask* is here. The split from the statement
//! walker is by size, not by principle: both halves are the same evaluator.

use kira_syntax_model::ast::{BinaryOp, CallArg, Expr, ExprId, UnaryOp};

use super::{EvalError, Evaluator, FIELD_TYPE};
use crate::diagnostics;
use crate::ksl;
use crate::syntax_ops::{self, SyntaxError};
use crate::value::{DeclarationValue, Value};

impl Evaluator<'_> {
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
                let value = self.value(base)?;
                member(&value, &name)
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
            _ => Err(EvalError::unsupported(format!(
                "a call to `{callee}` with {} argument(s)",
                values.len()
            ))),
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
        method_on(&value, method, &values)
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
                Ok(Value::Syntax(parts.join(separator)))
            }
            ("Diagnostics", "error", [Value::Str(message), ..]) => {
                self.reported.push(message.clone());
                Ok(Value::Void)
            }
            (ksl::NAMESPACE, "compile", values) => ksl::compile(self.shaders, values),
            // What is being built, answered at compile time. A program that
            // asked a C library which platform it was on would be asking at run
            // time a question the compiler already answered.
            ("Build", "platform", []) => Ok(Value::Str(self.platform.clone())),
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
        (Value::Declaration(declaration), "syntax") => {
            Ok(Value::Syntax(declaration.syntax.clone()))
        }
        (Value::Field(field), "name") => Ok(Value::Identifier(field.name.clone())),
        (Value::Field(field), FIELD_TYPE) => Ok(Value::TypeRef(field.type_text.clone())),
        (Value::Field(field), "initializer") => Ok(Value::Syntax(field.initializer.clone())),
        (Value::Field(field), "syntax") => Ok(Value::Syntax(field.syntax.clone())),
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
        (Value::Identifier(name), "asString", []) => Ok(Value::Str(name.clone())),
        (Value::TypeRef(written), "asSyntax", []) => Ok(Value::Syntax(written.clone())),
        (Value::Str(text), "asString", []) => Ok(Value::Str(text.clone())),
        (Value::Array(items), "count" | "len", []) => Ok(Value::Int(items.len() as i64)),
        (Value::Field(field), "hasAnnotation", [Value::Str(name)]) => {
            Ok(Value::Bool(field.has_annotation(name)))
        }
        (Value::Syntax(text), "identifiers", []) => Ok(Value::Array(
            syntax_ops::identifiers(text)
                .into_iter()
                .map(Value::Identifier)
                .collect(),
        )),
        (Value::Syntax(text), "dropField", [name]) => {
            let field = text_of(name)?;
            syntax_ops::drop_field(text, &field)
                .map(Value::Syntax)
                .map_err(syntax_error)
        }
        (Value::Syntax(text), "replaceIdentifier", [from, to]) => {
            let from = text_of(from)?;
            let to = text_of(to)?;
            Ok(Value::Syntax(crate::rename::every(
                text,
                &std::iter::once((from, to)).collect(),
            )))
        }
        (Value::Syntax(text), "rewriteProperty", [name, read, write]) => {
            let name = text_of(name)?;
            let read = text_of(read)?;
            let write = text_of(write)?;
            syntax_ops::rewrite_property(text, &name, &read, &write)
                .map(Value::Syntax)
                .map_err(syntax_error)
        }
        (other, method, args) => Err(EvalError::unsupported(format!(
            "`{method}` on a `{}` with {} argument(s)",
            other.type_name(),
            args.len()
        ))),
    }
}

/// The source text a `Syntax`, `Identifier`, `TypeRef`, or `String` carries.
fn text_of(value: &Value) -> Result<String, EvalError> {
    match value {
        Value::Syntax(text) | Value::Identifier(text) | Value::TypeRef(text) | Value::Str(text) => {
            Ok(text.clone())
        }
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
        Value::Str(text) | Value::Syntax(text) | Value::Identifier(text) | Value::TypeRef(text) => {
            Some(text)
        }
        _ => None,
    }
}

/// Builds the reflection value a macro's `Declaration` parameter is bound to.
pub(crate) fn declaration_value(declaration: &crate::decl::Declaration) -> Value {
    Value::Declaration(Box::new(DeclarationValue::of(declaration)))
}
