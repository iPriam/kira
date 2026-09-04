//! Evaluating a `comptime function` call during expansion.
//!
//! A `comptime function` is ordinary Kira that runs while the program is being
//! compiled. Its body goes on the same evaluator a `comptime macro`'s `expand`
//! runs on — so it has `let`, `if`, `while`, `for`, `match` and `attempt`, and
//! it reads the same reflection namespaces. What differs is only the two ends:
//! its arguments arrive as **values** rather than as the source text a macro's
//! fragment parameter carries, and what it returns becomes a **literal** at the
//! call site rather than syntax to splice.
//!
//! That is also why it needs no `!`. A macro call is written `name!(…)` because
//! what happens there is code substitution and a reader should see it; a
//! comptime function's call site is a value, indistinguishable from having
//! written the answer out, so it reads as the ordinary call it is.
//!
//! An argument is itself evaluated here, which is what lets one comptime
//! function call another and lets a call be written with arithmetic in it. An
//! argument the evaluator cannot fold is a refusal, never a guess: a call that
//! silently survived into the backend would be a runtime call to a function that
//! does not exist there.

use kira_source::Span;

use crate::diagnostics::{self, Reporter};
use crate::eval;
use crate::invoke::Invocation;
use crate::registry::ComptimeFunction;
use crate::tokens::Lexed;
use crate::value::Value;

/// Evaluates one call to `declared`, returning the literal to write over it.
pub(crate) fn expand_call(
    file: &Lexed<'_>,
    declared: &ComptimeFunction,
    call: &Invocation,
    comptime: eval::Comptime<'_>,
    reporter: &mut Reporter,
) -> Option<String> {
    if call.arguments.len() != declared.parameters.len() {
        reporter.error(
            file.source,
            call.name_span,
            diagnostics::EXPAND_SIGNATURE,
            format!(
                "`{}` takes {} argument(s), and this call passes {}",
                declared.name,
                declared.parameters.len(),
                call.arguments.len()
            ),
        );
        return None;
    }
    let mut arguments = Vec::with_capacity(call.arguments.len());
    for (parameter, argument) in declared.parameters.iter().zip(&call.arguments) {
        let value = evaluate(
            file,
            file.slice(*argument).trim(),
            *argument,
            comptime,
            reporter,
        )?;
        arguments.push((parameter.clone(), value));
    }
    let value = run_body(
        file,
        declared,
        arguments,
        call.name_span,
        comptime,
        reporter,
    )?;
    value.splice().or_else(|| {
        reporter.error(
            file.source,
            call.name_span,
            diagnostics::NO_SPLICE_RULE,
            format!(
                "`{}` answered with a `{}`, which has no literal spelling to write at a call site",
                declared.name,
                value.type_name()
            ),
        );
        None
    })
}

/// Evaluates one argument's source text to a value.
fn evaluate(
    file: &Lexed<'_>,
    text: &str,
    span: Span,
    comptime: eval::Comptime<'_>,
    reporter: &mut Reporter,
) -> Option<Value> {
    let synthesized = format!("return {text}");
    let body = match eval::compile(&synthesized) {
        Ok(body) => body,
        // The lifted text is `return ` plus the expression, so an opener's
        // offset maps back by that prefix; the span covers the expression.
        Err(eval::BodyError::Lift { offset, message, further }) => {
            let at = span.start as usize + offset.saturating_sub("return ".len());
            let line = text[..offset.saturating_sub("return ".len()).min(text.len())]
                .matches('\n')
                .count()
                + 1;
            let and_more = match further {
                0 => String::new(),
                _ => format!(" ({further} more follow it)"),
            };
            reporter.error(
                file.source,
                Span::from_bounds(at as u32, at as u32 + 1),
                diagnostics::UNCLOSED_QUOTE,
                format!("`{text}` has {message} at line {line}{and_more}"),
            );
            return None;
        }
        Err(eval::BodyError::Parse) => {
            reporter.error(
                file.source,
                span,
                diagnostics::UNSUPPORTED_IN_EXPAND,
                format!("`{text}` is not an expression the compile-time evaluator can read"),
            );
            return None;
        }
    };
    match eval::run_value(&body, Vec::new(), comptime, false) {
        Ok((value, _)) => Some(value),
        Err(error) => {
            reporter.error(file.source, span, error.code, error.message);
            None
        }
    }
}

/// Runs the declared body with `arguments` bound to its parameters.
fn run_body(
    file: &Lexed<'_>,
    declared: &ComptimeFunction,
    arguments: Vec<(String, Value)>,
    span: Span,
    comptime: eval::Comptime<'_>,
    reporter: &mut Reporter,
) -> Option<Value> {
    let body = match eval::compile(&declared.body) {
        Ok(body) => body,
        Err(error) => {
            error.report(
                reporter,
                declared.source,
                &declared.body,
                declared.body_span,
                &format!("the body of `comptime function {}`", declared.name),
            );
            return None;
        }
    };
    match eval::run_value(&body, arguments, comptime, false) {
        Ok((value, reported)) => {
            let failed = reported
                .iter()
                .any(|report| report.severity == kira_diagnostics::Severity::Error);
            for report in reported {
                let (source, at) = report
                    .at
                    .map_or((file.source, span), |at| (at.source, at.span));
                reporter.coded(
                    report.severity,
                    source,
                    at,
                    report.code.as_deref(),
                    report.fix.as_deref(),
                    report.message,
                );
            }
            if failed { None } else { Some(value) }
        }
        Err(error) => {
            reporter.error(file.source, span, error.code, error.message);
            None
        }
    }
}
