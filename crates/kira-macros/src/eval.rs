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

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::registry::ComptimeFunction;

use kira_core::Names;
use kira_diagnostics::Severity;
use kira_source::{FileSpan, SourceId, Span};
use kira_syntax_model::SyntaxTree;
use kira_syntax_model::ast::{
    BinaryOp, Block, Expr, ExprId, ForIterable, Item, MatchArm, MatchPattern, Stmt, StmtId,
};

use crate::diagnostics;
use crate::ksl::ShaderCompiler;
use crate::quote::{self, Chunk, Template};
use crate::value::Value;

pub(crate) mod methods;
pub(crate) mod reflection;
mod statements;

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

/// Why an `expand` body never became runnable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BodyError {
    /// A `quote { … }` or `#{ … }` that never closes, with its byte offset in
    /// the body, what it was, and how many more follow it.
    Lift {
        /// The unclosed opener's byte offset in the `expand` body.
        offset: usize,
        /// What never closed.
        message: String,
        /// Further lift failures after this one.
        further: usize,
    },
    /// A body that lifts but does not parse.
    Parse,
}

impl BodyError {
    /// Reports a body that never became runnable.
    ///
    /// `body_span` covers the body text in the compiled file, so a lift
    /// failure points at the opener the author wrote; `whose` names the body,
    /// as in "the `expand` body of `Serializable`".
    pub(crate) fn report(
        self,
        reporter: &mut diagnostics::Reporter,
        source: SourceId,
        body: &str,
        body_span: Span,
        whose: &str,
    ) {
        match self {
            BodyError::Lift {
                offset,
                message,
                further,
            } => {
                let at = body_span.start as usize + offset.min(body.len());
                let line = body[..offset.min(body.len())].matches('\n').count() + 1;
                let and_more = match further {
                    0 => String::new(),
                    _ => format!(" ({further} more follow it)"),
                };
                reporter.error(
                    source,
                    Span::from_bounds(at as u32, at as u32 + 1),
                    diagnostics::UNCLOSED_QUOTE,
                    format!("{whose} has {message} at line {line}{and_more}"),
                );
            }
            BodyError::Parse => {
                reporter.error(
                    source,
                    body_span,
                    diagnostics::EXPAND_SIGNATURE,
                    format!("{whose} does not parse"),
                );
            }
        }
    }
}

/// A parsed `expand` body, ready to run.
pub(crate) struct Body {
    tree: SyntaxTree,
    interner: Names,
    block: Block,
    templates: Vec<Template>,
}

/// Parses `text` as an `expand` body.
///
/// Lift failures name the unclosed opener; a body that lifts but does not
/// parse is reported by the caller as a malformed `expand`.
pub(crate) fn compile(text: &str) -> Result<Body, BodyError> {
    let (lifted, templates, lift_errors) = quote::lift(text);
    if let Some(first) = lift_errors.first() {
        return Err(BodyError::Lift {
            offset: first.offset,
            message: first.message().to_owned(),
            further: lift_errors.len() - 1,
        });
    }
    let source = format!(
        "function __kmac_expand() {{\n{}\n}}\n",
        rewrite_type_member(&lifted)
    );
    let parsed = kira_parser::parse(SourceId::new(0), &source);
    if kira_diagnostics::has_errors(&parsed.diagnostics) {
        return Err(BodyError::Parse);
    }
    let Some(Item::Function(function)) = parsed.tree.items().first() else {
        return Err(BodyError::Parse);
    };
    Ok(Body {
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

/// One problem a macro body raised about the code it was handed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Report {
    /// How serious the body said it was.
    ///
    /// A macro that *refuses* reports an error and its expansion is discarded.
    /// A macro that merely *observes* — a lint — reports a warning or a note,
    /// and what it returned is still spliced, because nothing about the code
    /// was wrong enough to drop.
    pub(crate) severity: Severity,
    /// What the body said.
    pub(crate) message: String,
    /// Where it said to point, from the `at:` argument — or `None` when it
    /// named nothing, or named something that came from no file.
    ///
    /// A caller with `None` here falls back to the macro's own declaration,
    /// which is the honest second-best: the macro is the only thing left that
    /// is certainly written somewhere.
    pub(crate) at: Option<FileSpan>,
    /// The code it reported under, from the `code:` argument.
    ///
    /// A lint names itself — `KLINT014` — and that name is what a reader
    /// suppresses by, so it has to reach the diagnostic rather than being
    /// flattened into every macro sharing one code. `None` falls back to
    /// [`MACRO_REPORTED`](crate::diagnostics::MACRO_REPORTED), which is the
    /// honest answer for a macro that did not name one.
    pub(crate) code: Option<String>,
    /// The text to write over [`Report::at`], from a `fix:` argument.
    ///
    /// A lint that can say what is wrong and not what to write instead is half
    /// a lint: the reader has to redo the analysis by hand. `Some` here is the
    /// macro claiming the replacement preserves behaviour, which is what makes
    /// it machine-applicable.
    pub(crate) fix: Option<String>,
}

/// What running an `expand` body produced.
#[derive(Debug, Default)]
pub(crate) struct Outcome {
    /// The syntax the body returned, rendered to source.
    pub(crate) syntax: String,
    /// Every problem the body raised with `Diagnostics.error`.
    pub(crate) reported: Vec<Report>,
}

/// Runs `body` with `arguments` bound to its `expand` parameters.
pub(crate) fn run(
    body: &Body,
    arguments: Vec<(String, Value)>,
    comptime: Comptime<'_>,
    lint: bool,
) -> Result<Outcome, EvalError> {
    let (value, reported) = run_value(body, arguments, comptime, lint)?;
    let syntax = match value {
        Value::Void => String::new(),
        other => other.splice().ok_or_else(|| {
            EvalError::coded(
                diagnostics::NO_SPLICE_RULE,
                format!("`expand` must return `Syntax`, not `{}`", other.type_name()),
            )
        })?,
    };
    Ok(Outcome { syntax, reported })
}

/// Runs `body` and hands back the value it returned, unspliced.
///
/// What a `comptime function` needs: its result becomes a literal at the call
/// site, and its arguments are themselves values rather than the source text a
/// macro's fragment parameter carries. [`run`] is this plus the splice a macro
/// wants.
pub(crate) fn run_value(
    body: &Body,
    arguments: Vec<(String, Value)>,
    comptime: Comptime<'_>,
    lint: bool,
) -> Result<(Value, Vec<Report>), EvalError> {
    run_value_shared(body, arguments, comptime, lint, Fuel::default())
}

/// Runs `body` against a step budget another evaluation already started.
///
/// What a nested evaluation needs — a `comptime function` a body called, or a
/// declaration member a lint read — so a runaway callee spends the caller's
/// fuel rather than minting fresh fuel per call and outrunning the limit by
/// nesting.
pub(crate) fn run_value_shared(
    body: &Body,
    arguments: Vec<(String, Value)>,
    comptime: Comptime<'_>,
    lint: bool,
    fuel: Fuel,
) -> Result<(Value, Vec<Report>), EvalError> {
    run_nested(body, arguments, comptime, lint, 0, fuel)
}

/// The comptime functions in scope during an evaluation, by name.
pub(crate) type ComptimeFunctions = HashMap<String, ComptimeFunction>;

/// What a compile-time body reaches besides its own arguments.
///
/// The compile-time inputs travel together through every layer that can run one — a macro's
/// `expand`, a `comptime function`, and each nested call either makes — so they
/// are carried as one value rather than threaded as three parameters that no
/// call site ever varies independently.
#[derive(Clone, Copy)]
pub(crate) struct Comptime<'a> {
    /// Every `comptime function` the program declares.
    pub(crate) functions: &'a ComptimeFunctions,
    /// The KSL pipeline the `Ksl` namespace reaches, or `None` when the caller
    /// supplied none.
    pub(crate) shaders: Option<&'a dyn ShaderCompiler>,
    /// The target platform the `Target` namespace answers for.
    pub(crate) platform: &'a str,
    /// Every enum the program declares, so a body may name one of its cases.
    pub(crate) enums: &'a HashMap<String, Vec<String>>,
    /// Whether the compiler is generating the `kira test` entrypoint.
    pub(crate) testing: bool,
}

/// How deep one comptime call may nest inside another.
const CALL_DEPTH_LIMIT: u32 = 32;

/// How many statements one comptime evaluation may run, nested calls included.
///
/// A `while` already caps its own rounds and calls cap their depth, but neither
/// bounds the whole run: a collector is one evaluation over every declaration
/// in the program, so a loop that never exits — or one slow enough to look
/// that way over a big program — would otherwise hang the compiler with no
/// diagnostic. Whatever the shape, evaluation stops here under `KMAC010`.
const STEP_LIMIT: u64 = 10_000_000;

/// How many value cells one comptime evaluation may build, nested calls
/// included.
///
/// Steps bound the rounds; this bounds what each round may build. Reads spend
/// nothing — answering from what is there is linear in the program — but every
/// fresh value does: concatenation, splits, replacements, renders, joins, body
/// parses, and pushed items. A loop whose values grow without bound spends
/// geometrically and stops here under `KMAC010`, while a linear pass over a big
/// program spends its size a small number of times over.
const CELL_LIMIT: u64 = 1_000_000_000;

/// Steps spent and cells built by one comptime evaluation, shared with every
/// nested call.
///
/// Reference-counted so a callee spends its caller's fuel: minting a fresh
/// budget per nested call would let a recursive macro outrun the limit by
/// nesting rather than by looping.
#[derive(Debug, Default, Clone)]
pub(crate) struct Fuel(Rc<FuelCounts>);

/// The counters one evaluation and its nested calls share.
#[derive(Debug, Default)]
struct FuelCounts {
    /// Statements executed, loop iterations included.
    steps: Cell<u64>,
    /// Value cells built by this evaluation and its nested calls.
    cells: Cell<u64>,
}

fn run_nested(
    body: &Body,
    arguments: Vec<(String, Value)>,
    comptime: Comptime<'_>,
    lint: bool,
    depth: u32,
    fuel: Fuel,
) -> Result<(Value, Vec<Report>), EvalError> {
    let mut evaluator = Evaluator {
        body,
        functions: comptime.functions,
        depth,
        scopes: vec![arguments.into_iter().collect()],
        reported: Vec::new(),
        shaders: comptime.shaders,
        platform: comptime.platform.to_owned(),
        enums: comptime.enums.clone(),
        testing: comptime.testing,
        lint,
        fuel,
    };
    let value = match evaluator.block(&body.block)? {
        Flow::Return(value) => value,
        Flow::Normal | Flow::Break | Flow::Continue => Value::Void,
    };
    Ok((value, evaluator.reported))
}

/// How one statement of an `attempt` body finished.
enum Attempted {
    /// It ran; this is how it left.
    Ran(Flow),
    /// A `try` in it unwrapped the failure case, which the handlers route.
    Failed(crate::value::EnumCaseValue),
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
    /// The `comptime function`s the program declares, so one can call another.
    ///
    /// Composition is the point: a comptime function that could not call its
    /// neighbours would be a single expression wearing a declaration's clothes.
    functions: &'a ComptimeFunctions,
    /// How many comptime calls deep this evaluation already is, so a function
    /// that calls itself is refused rather than hanging the compiler.
    depth: u32,
    scopes: Vec<HashMap<String, Value>>,
    reported: Vec<Report>,
    /// The KSL pipeline `Ksl.compile` reaches, when one was supplied.
    shaders: Option<&'a dyn ShaderCompiler>,
    /// The operating system this build targets, for `Build.platform`.
    platform: String,
    /// Every enum the program declares, by name, with its case names.
    enums: HashMap<String, Vec<String>>,
    /// Whether `kira lint` asked for this collection, for `Build.linting`.
    ///
    /// Only a collector is told: it is the one macro form a verb runs *for*,
    /// and the only one that has any business asking which verb that was.
    lint: bool,
    /// Whether the compiler is generating the `kira test` entrypoint.
    testing: bool,
    /// Steps this evaluation has left to spend, shared with nested calls.
    fuel: Fuel,
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

    /// Spends one evaluation step, refusing when the budget is gone.
    ///
    /// Charged once per statement a body executes, loop iterations included,
    /// so this is what stops a loop that never exits: whatever it does per
    /// round, the rounds themselves are counted.
    fn charge(&mut self) -> Result<(), EvalError> {
        let spent = self.fuel.0.steps.get().saturating_add(1);
        self.fuel.0.steps.set(spent);
        if spent > STEP_LIMIT {
            return Err(EvalError::coded(
                diagnostics::DEPTH_LIMIT,
                format!(
                    "comptime evaluation ran more than {STEP_LIMIT} steps without returning; a loop that never exits stops the build here rather than hanging the compiler"
                ),
            ));
        }
        Ok(())
    }

    /// Spends `cells` of the build budget, refusing when it is gone.
    ///
    /// Charged for fresh values — concatenation, splits, renders, parses, and
    /// pushes — never for reads, so a linear pass spends its size and a loop
    /// whose values grow without bound spends geometrically.
    fn charge_cells(&mut self, cells: u64) -> Result<(), EvalError> {
        let spent = self.fuel.0.cells.get().saturating_add(cells);
        self.fuel.0.cells.set(spent);
        if spent > CELL_LIMIT {
            return Err(EvalError::coded(
                diagnostics::DEPTH_LIMIT,
                format!(
                    "comptime evaluation built more than {CELL_LIMIT} cells of values without returning; a loop whose values grow without bound stops the build here rather than hanging the compiler"
                ),
            ));
        }
        Ok(())
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
        // A `quote` is assembled from a template and its splices, so it is
        // written in the macro rather than in any file the macro is looking at.
        Ok(Value::built(out))
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
        // `try` reaching here means it was written somewhere other than as the
        // whole initializer of a `let` inside an `attempt`, which is the one
        // position the language accepts it in either.
        Expr::Try { .. } => "`try` outside a `let` in an `attempt`",
        _ => "expression",
    }
}

/// A short name for a statement form, for the unsupported-construct message.
fn statement_shape(stmt: &Stmt) -> &'static str {
    match stmt {
        Stmt::Match { .. } => "`match`",
        Stmt::Attempt { .. } => "`attempt`",
        _ => "that statement",
    }
}

impl Evaluator<'_> {
    /// Runs a `comptime function` the body called, when `callee` names one.
    pub(super) fn call_comptime(
        &mut self,
        callee: &str,
        values: &[Value],
    ) -> Option<Result<Value, EvalError>> {
        let declared = self.functions.get(callee)?;
        if declared.parameters.len() != values.len() {
            return None;
        }
        if self.depth >= CALL_DEPTH_LIMIT {
            return Some(Err(EvalError::coded(
                diagnostics::DEPTH_LIMIT,
                format!(
                    "`{callee}` nested more than {CALL_DEPTH_LIMIT} comptime calls deep; a                      comptime function that calls itself has no base case here"
                ),
            )));
        }
        let body = match compile(&declared.body) {
            Ok(body) => body,
            Err(BodyError::Lift {
                offset,
                message,
                further,
            }) => {
                let line = declared.body[..offset.min(declared.body.len())]
                    .matches('\n')
                    .count()
                    + 1;
                let and_more = match further {
                    0 => String::new(),
                    _ => format!(" ({further} more follow it)"),
                };
                return Some(Err(EvalError::coded(
                    diagnostics::UNCLOSED_QUOTE,
                    format!(
                        "the body of `comptime function {callee}` has {message} at line {line}{and_more}"
                    ),
                )));
            }
            Err(BodyError::Parse) => {
                return Some(Err(EvalError::coded(
                    diagnostics::EXPAND_SIGNATURE,
                    format!("the body of `comptime function {callee}` does not parse"),
                )));
            }
        };
        let bound: Vec<(String, Value)> = declared
            .parameters
            .iter()
            .cloned()
            .zip(values.iter().cloned())
            .collect();
        let comptime = Comptime {
            functions: self.functions,
            shaders: self.shaders,
            platform: &self.platform.clone(),
            enums: &self.enums.clone(),
            testing: self.testing,
        };
        match run_nested(
            &body,
            bound,
            comptime,
            self.lint,
            self.depth + 1,
            self.fuel.clone(),
        ) {
            Ok((value, reported)) => {
                self.reported.extend(reported);
                Some(Ok(value))
            }
            Err(error) => Some(Err(error)),
        }
    }
}

/// The `quote` template a callee names, if it names one.
pub(crate) fn template_of(callee: &str) -> Option<usize> {
    quote::template_id(callee)
}

#[cfg(test)]
mod tests;
