//! The compile-time evaluator: running a `comptime macro`'s `expand` body.
//!
//! The body is ordinary Kira, so it is parsed by the ordinary parser and walked
//! here over [`Value`]s. Two things are lifted out before parsing, because
//! neither is expressible in Kira's grammar: `quote { … }` becomes a call to a
//! synthetic template (see [`crate::quote`]), and `.type` — `type` is a
//! keyword — becomes a member the parser accepts.
//!
//! Anything the evaluator does not implement is [`KMAC020`], never a guess: a
//! macro that miscompiled silently would be worse than one that refuses.
//!
//! [`KMAC020`]: crate::diagnostics::UNSUPPORTED_IN_EXPAND

use std::collections::HashMap;

use kira_core::Interner;
use kira_source::SourceId;
use kira_syntax_model::SyntaxTree;
use kira_syntax_model::ast::{Block, Expr, ExprId, ForIterable, Item, Stmt, StmtId};

use crate::diagnostics;
use crate::ksl::ShaderCompiler;
use crate::quote::{self, Chunk, Template};
use crate::value::Value;

pub(crate) mod methods;

/// The member name `.type` is rewritten to before parsing.
pub(crate) const FIELD_TYPE: &str = "__kmac_field_type";

/// Why an `expand` body could not be run to a result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EvalError {
    /// The `KMAC` code to report it under.
    pub(crate) code: &'static str,
    /// What went wrong.
    pub(crate) message: String,
}

impl EvalError {
    /// An unsupported construct in an `expand` body.
    fn unsupported(what: impl Into<String>) -> Self {
        Self {
            code: diagnostics::UNSUPPORTED_IN_EXPAND,
            message: format!(
                "`expand` bodies run on the compile-time evaluator, which does not support {}",
                what.into()
            ),
        }
    }

    /// A failure with a specific code.
    pub(crate) fn coded(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// A parsed `expand` body, ready to run.
pub(crate) struct Body {
    tree: SyntaxTree,
    interner: Interner,
    block: Block,
    templates: Vec<Template>,
}

/// Parses `text` as an `expand` body.
///
/// `None` when the body does not parse at all, which is reported by the caller
/// as a malformed `expand`.
pub(crate) fn compile(text: &str) -> Option<Body> {
    let (lifted, templates) = quote::lift(text);
    let source = format!(
        "function __kmac_expand() {{\n{}\n}}\n",
        rewrite_type_member(&lifted)
    );
    let parsed = kira_parser::parse(SourceId::new(0), &source);
    if kira_diagnostics::has_errors(&parsed.diagnostics) {
        return None;
    }
    let Some(Item::Function(function)) = parsed.tree.items().first() else {
        return None;
    };
    Some(Body {
        block: function.body.clone(),
        tree: parsed.tree,
        interner: parsed.interner,
        templates,
    })
}

/// Rewrites `.type` to a member name the parser accepts.
fn rewrite_type_member(text: &str) -> String {
    let file = crate::tokens::Lexed::new(SourceId::new(0), text);
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0usize;
    for index in 1..file.len() {
        if file.kind(index) != kira_syntax_model::TokenKind::Type
            || file.kind(index - 1) != kira_syntax_model::TokenKind::Dot
        {
            continue;
        }
        let span = file.span(index);
        let start = (span.start as usize).min(text.len());
        let end = (span.end() as usize).min(text.len());
        out.push_str(text.get(cursor..start).unwrap_or(""));
        out.push_str(FIELD_TYPE);
        cursor = end;
    }
    out.push_str(text.get(cursor..).unwrap_or(""));
    out
}

/// What running an `expand` body produced.
#[derive(Debug, Default)]
pub(crate) struct Outcome {
    /// The syntax the body returned, rendered to source.
    pub(crate) syntax: String,
    /// Every message the body raised with `Diagnostics.error`.
    pub(crate) reported: Vec<String>,
}

/// Runs `body` with `arguments` bound to its `expand` parameters.
///
/// `shaders` is the KSL pipeline the `Ksl` namespace reaches, or `None` when
/// the caller supplied none.
pub(crate) fn run(
    body: &Body,
    arguments: Vec<(String, Value)>,
    shaders: Option<&dyn ShaderCompiler>,
) -> Result<Outcome, EvalError> {
    let mut evaluator = Evaluator {
        body,
        scopes: vec![arguments.into_iter().collect()],
        reported: Vec::new(),
        shaders,
    };
    let value = match evaluator.block(&body.block)? {
        Flow::Return(value) => value,
        Flow::Normal | Flow::Break | Flow::Continue => Value::Void,
    };
    let syntax = match value {
        Value::Void => String::new(),
        other => other.splice().ok_or_else(|| {
            EvalError::coded(
                diagnostics::NO_SPLICE_RULE,
                format!("`expand` must return `Syntax`, not `{}`", other.type_name()),
            )
        })?,
    };
    Ok(Outcome {
        syntax,
        reported: evaluator.reported,
    })
}

/// How a statement finished.
enum Flow {
    /// Fell through to the next statement.
    Normal,
    /// Returned a value.
    Return(Value),
    /// Left the innermost loop.
    Break,
    /// Skipped to the innermost loop's next iteration.
    Continue,
}

/// The running interpreter.
struct Evaluator<'a> {
    body: &'a Body,
    scopes: Vec<HashMap<String, Value>>,
    reported: Vec<String>,
    /// The KSL pipeline `Ksl.compile` reaches, when one was supplied.
    shaders: Option<&'a dyn ShaderCompiler>,
}

impl Evaluator<'_> {
    /// The text of a symbol.
    fn name(&self, symbol: kira_core::Symbol) -> &str {
        self.body.interner.resolve(symbol)
    }

    fn expr(&self, id: ExprId) -> &Expr {
        self.body.tree.expr(id)
    }

    fn stmt(&self, id: StmtId) -> &Stmt {
        self.body.tree.stmt(id)
    }

    fn lookup(&self, name: &str) -> Option<&Value> {
        self.scopes.iter().rev().find_map(|scope| scope.get(name))
    }

    fn bind(&mut self, name: &str, value: Value) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name.to_owned(), value);
        }
    }

    fn assign(&mut self, name: &str, value: Value) -> Result<(), EvalError> {
        for scope in self.scopes.iter_mut().rev() {
            if let Some(slot) = scope.get_mut(name) {
                *slot = value;
                return Ok(());
            }
        }
        Err(EvalError::unsupported(format!(
            "assigning to the unbound name `{name}`"
        )))
    }

    /// Runs a block in its own scope.
    fn block(&mut self, block: &Block) -> Result<Flow, EvalError> {
        self.scopes.push(HashMap::new());
        let mut flow = Flow::Normal;
        for &id in &block.stmts {
            flow = self.statement(id)?;
            if !matches!(flow, Flow::Normal) {
                break;
            }
        }
        self.scopes.pop();
        Ok(flow)
    }

    fn statement(&mut self, id: StmtId) -> Result<Flow, EvalError> {
        match self.stmt(id).clone() {
            Stmt::Let { name, init, .. } => {
                let value = self.value(init)?;
                let name = self.name(name).to_owned();
                self.bind(&name, value);
                Ok(Flow::Normal)
            }
            Stmt::Assign { target, value, .. } => {
                let evaluated = self.value(value)?;
                match self.expr(target).clone() {
                    Expr::Name { symbol, .. } => {
                        let name = self.name(symbol).to_owned();
                        self.assign(&name, evaluated)?;
                        Ok(Flow::Normal)
                    }
                    other => Err(EvalError::unsupported(format!(
                        "assigning to a {}",
                        shape(&other)
                    ))),
                }
            }
            Stmt::Return { value, .. } => {
                let returned = match value {
                    Some(id) => self.value(id)?,
                    None => Value::Void,
                };
                Ok(Flow::Return(returned))
            }
            Stmt::Expr { expr, .. } => {
                self.value(expr)?;
                Ok(Flow::Normal)
            }
            Stmt::If {
                cond,
                then_block,
                else_block,
                ..
            } => {
                let taken = self.condition(cond)?;
                if taken {
                    self.block(&then_block)
                } else if let Some(otherwise) = else_block {
                    self.block(&otherwise)
                } else {
                    Ok(Flow::Normal)
                }
            }
            Stmt::While { cond, body, .. } => {
                let mut rounds = 0u32;
                while self.condition(cond)? {
                    rounds += 1;
                    if rounds > LOOP_LIMIT {
                        return Err(EvalError::coded(
                            diagnostics::DEPTH_LIMIT,
                            format!(
                                "a `while` in an `expand` body ran more than {LOOP_LIMIT} times"
                            ),
                        ));
                    }
                    match self.block(&body)? {
                        Flow::Return(value) => return Ok(Flow::Return(value)),
                        Flow::Break => break,
                        Flow::Normal | Flow::Continue => {}
                    }
                }
                Ok(Flow::Normal)
            }
            Stmt::For {
                name,
                iterable,
                body,
                ..
            } => self.for_loop(name, &iterable, &body),
            Stmt::Break { .. } => Ok(Flow::Break),
            Stmt::Continue { .. } => Ok(Flow::Continue),
            other => Err(EvalError::unsupported(statement_shape(&other))),
        }
    }

    fn for_loop(
        &mut self,
        name: kira_core::Symbol,
        iterable: &ForIterable,
        body: &Block,
    ) -> Result<Flow, EvalError> {
        let name = self.name(name).to_owned();
        let items = match iterable {
            ForIterable::Range { start, end } => {
                let (Value::Int(from), Value::Int(to)) = (self.value(*start)?, self.value(*end)?)
                else {
                    return Err(EvalError::unsupported("a range over non-integers"));
                };
                (from..to).map(Value::Int).collect()
            }
            ForIterable::Each { array } => match self.value(*array)? {
                Value::Array(items) => items,
                other => {
                    return Err(EvalError::unsupported(format!(
                        "iterating a `{}`",
                        other.type_name()
                    )));
                }
            },
        };
        for item in items {
            self.scopes.push(HashMap::new());
            self.bind(&name, item);
            let flow = self.block(body);
            self.scopes.pop();
            match flow? {
                Flow::Return(value) => return Ok(Flow::Return(value)),
                Flow::Break => break,
                Flow::Normal | Flow::Continue => {}
            }
        }
        Ok(Flow::Normal)
    }

    fn condition(&mut self, id: ExprId) -> Result<bool, EvalError> {
        let value = self.value(id)?;
        value.as_bool().ok_or_else(|| {
            EvalError::unsupported(format!("a `{}` where a Bool is needed", value.type_name()))
        })
    }

    /// Renders quote template `id` with `arguments` spliced in.
    fn render(&self, id: usize, arguments: &[Value]) -> Result<Value, EvalError> {
        let Some(template) = self.body.templates.get(id) else {
            return Err(EvalError::unsupported("an unknown quote template"));
        };
        let mut out = String::new();
        for chunk in &template.chunks {
            match chunk {
                Chunk::Text(text) => out.push_str(text),
                Chunk::Splice(index) => {
                    let Some(value) = arguments.get(*index) else {
                        return Err(EvalError::unsupported("a quote splice with no value"));
                    };
                    let rendered = value.splice().ok_or_else(|| {
                        EvalError::coded(
                            diagnostics::NO_SPLICE_RULE,
                            format!("a `{}` has no `#{{ … }}` splice rule", value.type_name()),
                        )
                    })?;
                    out.push_str(&rendered);
                }
            }
        }
        Ok(Value::Syntax(out))
    }
}

/// How many iterations a `while` in an `expand` body may run.
const LOOP_LIMIT: u32 = 100_000;

/// A short name for an expression form, for the unsupported-construct message.
fn shape(expr: &Expr) -> &'static str {
    match expr {
        Expr::Name { .. } => "name",
        Expr::Field { .. } => "field",
        Expr::Index { .. } => "index",
        Expr::Call { .. } => "call",
        Expr::MethodCall { .. } => "method call",
        Expr::StructLit { .. } => "struct literal",
        Expr::Closure { .. } => "closure",
        _ => "expression",
    }
}

/// A short name for a statement form, for the unsupported-construct message.
fn statement_shape(stmt: &Stmt) -> &'static str {
    match stmt {
        Stmt::Switch { .. } => "`switch`",
        Stmt::Match { .. } => "`match`",
        Stmt::Attempt { .. } => "`attempt`",
        _ => "that statement",
    }
}

/// The `quote` template a callee names, if it names one.
pub(crate) fn template_of(callee: &str) -> Option<usize> {
    quote::template_id(callee)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_body(text: &str, arguments: Vec<(String, Value)>) -> Result<Outcome, EvalError> {
        let body = compile(text).expect("a parseable expand body");
        run(&body, arguments, None)
    }

    #[test]
    fn arithmetic_and_conditionals_run() {
        let outcome = run_body(
            "var total: Int = 0\nvar i: Int = 0\nwhile i < 4 {\n    total = total + i\n    i = i + 1\n}\nif total > 5 {\n    return quote { big }\n}\nreturn quote { small }\n",
            Vec::new(),
        )
        .expect("a result");
        assert_eq!(outcome.syntax.trim(), "big");
    }

    #[test]
    fn a_for_loop_over_an_array_binds_each_element() {
        let outcome = run_body(
            "var out: [Syntax] = []\nfor name in names {\n    out.append(quote { #{name} })\n}\nreturn quote { #{out} }\n",
            vec![(
                "names".to_owned(),
                Value::Array(vec![
                    Value::Identifier("a".to_owned()),
                    Value::Identifier("b".to_owned()),
                ]),
            )],
        )
        .expect("a result");
        assert!(outcome.syntax.contains('a'), "{}", outcome.syntax);
        assert!(outcome.syntax.contains('b'), "{}", outcome.syntax);
    }

    #[test]
    fn an_unsupported_statement_is_refused_rather_than_guessed() {
        let error = run_body("match x {\n    Red -> return quote { }\n}\n", Vec::new())
            .expect_err("a refusal");
        assert_eq!(error.code, "KMAC020");
    }
}
