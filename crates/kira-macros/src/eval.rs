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

use crate::registry::ComptimeFunction;

use kira_core::Names;
use kira_diagnostics::Severity;
use kira_source::{FileSpan, SourceId, Span};
use kira_syntax_model::SyntaxTree;
use kira_syntax_model::ast::{Block, Expr, ExprId, ForIterable, Item, MatchArm, Stmt, StmtId};

use crate::diagnostics;
use crate::ksl::ShaderCompiler;
use crate::quote::{self, Chunk, Template};
use crate::value::Value;

pub(crate) mod methods;
pub(crate) mod reflection;

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
            BodyError::Lift { offset, message, further } => {
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
    run_nested(body, arguments, comptime, lint, 0)
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

fn run_nested(
    body: &Body,
    arguments: Vec<(String, Value)>,
    comptime: Comptime<'_>,
    lint: bool,
    depth: u32,
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
            Stmt::Match { subject, arms, .. } => self.match_statement(subject, &arms),
            Stmt::Attempt { body, handlers, .. } => self.attempt_statement(&body, &handlers),
            Stmt::Break { .. } => Ok(Flow::Break),
            Stmt::Continue { .. } => Ok(Flow::Continue),
            other => Err(EvalError::unsupported(statement_shape(&other))),
        }
    }

    /// Runs an `attempt { … } handle { … }`.
    ///
    /// The body runs statement by statement until a `try` unwraps a case that
    /// turned out to be the failure one, at which point the rest of the body is
    /// skipped and the arm naming that failure runs instead — which is what the
    /// language does, and the reason statements after a `try` nest into its
    /// success branch there.
    ///
    /// `Result`-shaped is structural here as it is everywhere else: any case
    /// named `Ok` succeeds and carries the value on, any other case is the
    /// failure and is routed. Nothing nominal is required, so a body may `try`
    /// an enum it declared itself.
    fn attempt_statement(
        &mut self,
        body: &Block,
        handlers: &[MatchArm],
    ) -> Result<Flow, EvalError> {
        self.scopes.push(HashMap::new());
        let mut failure = None;
        let mut flow = Flow::Normal;
        for &id in &body.stmts {
            match self.try_statement(id)? {
                Attempted::Ran(next) => {
                    flow = next;
                    if !matches!(flow, Flow::Normal) {
                        break;
                    }
                }
                Attempted::Failed(case) => {
                    failure = Some(case);
                    break;
                }
            }
        }
        self.scopes.pop();
        let Some(case) = failure else {
            return Ok(flow);
        };
        for arm in handlers {
            if self.name(arm.variant) != case.variant {
                continue;
            }
            self.scopes.push(HashMap::new());
            if let Some(binding) = &arm.binding {
                let name = self.name(binding.name).to_owned();
                let payload = case.payload.clone().unwrap_or(Value::Void);
                self.bind(&name, payload);
            }
            let mut handled = Flow::Normal;
            for &id in &arm.body.stmts {
                handled = self.statement(id)?;
                if !matches!(handled, Flow::Normal) {
                    break;
                }
            }
            self.scopes.pop();
            return Ok(handled);
        }
        Err(EvalError::unsupported(format!(
            "an `attempt` with no handler for `{}`",
            case.variant
        )))
    }

    /// Runs one statement of an `attempt` body, reporting a `try` that failed.
    fn try_statement(&mut self, id: StmtId) -> Result<Attempted, EvalError> {
        let Stmt::Let { name, init, .. } = self.stmt(id).clone() else {
            return Ok(Attempted::Ran(self.statement(id)?));
        };
        let Expr::Try { value, .. } = self.expr(init).clone() else {
            return Ok(Attempted::Ran(self.statement(id)?));
        };
        let outcome = self.value(value)?;
        let Value::EnumCase(case) = outcome else {
            return Err(EvalError::unsupported(format!(
                "`try` on a `{}`; it unwraps a `Result`-shaped enum case",
                outcome.type_name()
            )));
        };
        if case.variant != "Ok" {
            return Ok(Attempted::Failed(*case));
        }
        let name = self.name(name).to_owned();
        self.bind(&name, case.payload.clone().unwrap_or(Value::Void));
        Ok(Attempted::Ran(Flow::Normal))
    }

    /// Runs a `match` over an enum case.
    ///
    /// An arm selects by variant name, which is all a case carries that matters
    /// here: a bare `.Variant` never knew its enum, so matching on the name is
    /// the only rule that works for both a case read from reflection and one the
    /// body wrote itself.
    ///
    /// A subject no arm names is an error rather than a fall-through. The
    /// language checks exhaustiveness before a program runs; an `expand` body is
    /// evaluated rather than compiled, so the equivalent guarantee has to be
    /// this — a macro that forgot a variant hears about it instead of silently
    /// producing nothing.
    fn match_statement(&mut self, subject: ExprId, arms: &[MatchArm]) -> Result<Flow, EvalError> {
        let value = self.value(subject)?;
        let Value::EnumCase(case) = value else {
            return Err(EvalError::unsupported(format!(
                "matching on a `{}`; `match` in an `expand` body selects a variant of an enum case",
                value.type_name()
            )));
        };
        for arm in arms {
            if self.name(arm.variant) != case.variant {
                continue;
            }
            self.scopes.push(HashMap::new());
            if let Some(binding) = &arm.binding {
                let name = self.name(binding.name).to_owned();
                let payload = case.payload.clone().unwrap_or(Value::Void);
                self.bind(&name, payload);
            }
            let mut flow = Flow::Normal;
            for &id in &arm.body.stmts {
                flow = self.statement(id)?;
                if !matches!(flow, Flow::Normal) {
                    break;
                }
            }
            self.scopes.pop();
            return Ok(flow);
        }
        Err(EvalError::unsupported(format!(
            "a `match` with no arm for `{}`",
            case.variant
        )))
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
                // Materialized lazily and bounded like a `while`: a range the
                // width of an address space would otherwise be collected up
                // front and end the compiler with an allocation failure
                // instead of this diagnostic.
                let count = to.saturating_sub(from).max(0);
                if count > i64::from(LOOP_LIMIT) {
                    return Err(EvalError::coded(
                        diagnostics::DEPTH_LIMIT,
                        format!(
                            "a `for` in an `expand` body ranges over more than {LOOP_LIMIT} items"
                        ),
                    ));
                }
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
            Err(BodyError::Lift { offset, message, further }) => {
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
                    format!("the body of `comptime function {callee}` has {message} at line {line}{and_more}"),
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
        match run_nested(&body, bound, comptime, self.lint, self.depth + 1) {
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
mod tests {
    use super::*;

    fn run_body(text: &str, arguments: Vec<(String, Value)>) -> Result<Outcome, EvalError> {
        let body = compile(text).expect("a parseable expand body");
        let functions = ComptimeFunctions::new();
        let enums = HashMap::new();
        let comptime = Comptime {
            functions: &functions,
            shaders: None,
            platform: "unknown",
            enums: &enums,
            testing: false,
        };
        run(&body, arguments, comptime, false)
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

    /// `Identifier(text)` builds the use-site identifier the text spells, so a
    /// macro can name what only exists as a string — one branch per spelling
    /// is the alternative, and it stops scaling at two.
    #[test]
    fn an_identifier_built_from_text_splices_as_a_name() {
        let outcome = run_body(
            "var name: Identifier = Identifier(\"answer\")\nreturn quote { #{name} }",
            Vec::new(),
        )
        .expect("a result");
        assert_eq!(outcome.syntax.trim(), "answer");
    }

    /// Text no identifier can spell is refused where the text is still
    /// visible, not where the name would have landed.
    #[test]
    fn an_identifier_built_from_a_keyword_or_a_non_name_is_refused() {
        for text in ["return", "9lives", "", "has space", "has-dash"] {
            let error = run_body(
                &format!("var name: Identifier = Identifier(\"{text}\")\nreturn quote {{ x }}"),
                Vec::new(),
            )
            .expect_err("a refusal");
            assert_eq!(error.code, "KMAC013", "{text}");
        }
    }
}
