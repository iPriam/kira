//! The `attempt`/`try`/`handle` desugar: a guarded body into the nested
//! `if`/`else` chain that [`matches`](super::matches) already builds.
//!
//! # What this costs the backends
//!
//! Nothing. An `attempt` becomes [`HirStmt::If`] over an `Int` tag comparison,
//! exactly like a `match`, and reuses that module's arm resolution, chain
//! builder, and payload projection verbatim. No IR node, no opcode, no VM
//! dispatch arm, no LLVM helper, and no WASM lowering learns that `attempt`,
//! `try`, or `handle` exists.
//!
//! Given `attempt { let v = try f(n); return v * 2 } handle { A { P } B { Q } }`:
//!
//! ```text
//! let <result> = f(n)                 // hidden: evaluated once
//! let <rtag>   = EnumTag(<result>)
//! if <rtag> == <tag of `Error`> {
//!     let <failure> = EnumPayload(<result>)
//!     let <ftag>    = EnumTag(<failure>)
//!     if <ftag> == 0 { P } else { Q }  // the handlers, an exhaustive chain
//! } else {
//!     let v = EnumPayload(<result>)    // the `Ok` payload, as written
//!     return v * 2                     // the rest of the body, nested here
//! }
//! ```
//!
//! # Why the rest of the body nests inside the `else`
//!
//! A `try` is an early exit, and the HIR has no early exit that is not a
//! `return`. So the statements *after* a `try` are precisely the statements
//! that run when it succeeded — which makes them the `else` branch. Lowering is
//! therefore recursive over the body's statement list rather than a loop: each
//! `try` consumes the remainder of the list into its own success branch.
//!
//! That shape is also what makes the reference's `emxProcess` a function that
//! definitely returns with no trailing `return`: `body_definitely_returns`
//! wants both branches of an `if` to return, and here both do.
//!
//! # Why `try` is accepted in one position only
//!
//! Only as the entire initializer of a `let` directly inside an `attempt` body.
//! The reference's own diagnostic is "`try` outside `attempt` **or in an
//! unsupported position**", and its corpus writes exactly one spelling —
//! `let <name> = try <expr>`. Accepting `try` in an arbitrary expression
//! position would mean inventing an answer for `g(try f(), try h())`, which
//! nothing pins. So every other position is refused rather than guessed at.
//!
//! # Why a handler chain is emitted per `try`
//!
//! Two `try`s in one body produce two copies of the handler arms. The
//! alternative — one shared chain reached by a flag — needs a jump the HIR
//! cannot express without inventing a loop, and the reference requires all
//! `try`s of one `attempt` to share a single failure enum precisely so the arms
//! *can* be repeated. Duplication is the cheaper of the two, and it is
//! invisible below this module.

use kira_semantics_model::hir::{HirExpr, HirStmt, HirStmtId, LocalId};
use kira_semantics_model::{EnumId, Type};
use kira_source::Span;
use kira_syntax_model::ast::{Block, Expr, ExprId, MatchArm, Stmt, StmtId, TypeRefId};

use crate::analyze::{Analyzer, FnCtx};
use crate::stmt::matches::ResolvedArm;

/// The `Ok`/`Error` pair a `try` operand has to have.
struct ResultShape {
    /// Discriminant of the `Error` variant.
    error_tag: u32,
    /// The failure enum, taken from the `Error` variant's payload.
    failure: EnumId,
    /// The `Ok` variant's payload type, which is what the `let` binds.
    ok_payload: Type,
}

/// One `try`'s result value: where it lives and what shape it has.
struct TryFrame {
    /// The hidden local holding the `Result`-shaped value.
    slot: LocalId,
    /// That local's type.
    result_ty: Type,
    /// Its `Ok`/`Error` shape.
    shape: ResultShape,
}

/// What every `try` in one `attempt` shares: the handlers it routes to, the
/// failure enum they belong to, and where to report against.
struct Guard<'a> {
    /// The handler arms, in source order.
    handlers: &'a [MatchArm],
    /// The failure enum the first resolved `try` settled on, and where it was
    /// written — `None` until one resolves.
    agreed: &'a mut Option<(EnumId, Span)>,
    /// Span of the whole `attempt`, which coverage reports point at.
    attempt_span: Span,
    /// Whether a `try` was *written*, as opposed to one that resolved.
    ///
    /// Tracked apart from `agreed` so a `try` on something that is not
    /// `Result`-shaped reports only that, instead of also being told the body
    /// contains no `try` at all — one mistake, one diagnostic.
    saw_try: bool,
}

impl Analyzer<'_> {
    /// Analyzes an `attempt`, appending the chain it desugars to onto `out`.
    pub(crate) fn analyze_attempt(
        &mut self,
        ctx: &mut FnCtx,
        body: &Block,
        handlers: &[MatchArm],
        span: Span,
        out: &mut Vec<HirStmtId>,
    ) {
        let mut agreed = None;
        let mut guard = Guard {
            handlers,
            agreed: &mut agreed,
            attempt_span: span,
            saw_try: false,
        };
        ctx.push_scope();
        let lowered = self.lower_guarded(ctx, &body.stmts, &mut guard);
        let guard_saw_try = guard.saw_try;
        ctx.pop_scope();
        out.extend(lowered);

        // No `try` means the handlers name variants of an enum nothing chose,
        // so there is nothing to resolve them against. The reference does not
        // pin this program, so it is refused rather than guessed at.
        if !guard_saw_try {
            self.emit(
                span,
                "KSEM143",
                "an `attempt` body must contain a `try`".to_owned(),
            );
        }
    }

    /// Lowers a run of statements, splitting at the first `try` and nesting
    /// everything after it into that `try`'s success branch.
    fn lower_guarded(
        &mut self,
        ctx: &mut FnCtx,
        stmts: &[StmtId],
        guard: &mut Guard<'_>,
    ) -> Vec<HirStmtId> {
        let mut out = Vec::new();
        for (index, &stmt_id) in stmts.iter().enumerate() {
            let Some(guarded) = self.as_guarded_let(stmt_id) else {
                self.analyze_stmt(ctx, stmt_id, &mut out);
                continue;
            };
            let rest = &stmts[index + 1..];
            self.lower_try(ctx, &guarded, rest, guard, &mut out);
            return out;
        }
        out
    }

    /// Recognizes `let <name> = try <expr>` — the one position a `try` is
    /// accepted in.
    ///
    /// Returns `None` for every other statement, including a `let` whose
    /// initializer merely *contains* a `try`; that `try` is reached through
    /// ordinary expression analysis instead, which reports it.
    fn as_guarded_let(&self, stmt_id: StmtId) -> Option<GuardedLet> {
        let Stmt::Let {
            name,
            name_span,
            mutable,
            ty,
            init,
            ..
        } = self.tree.stmt(stmt_id).clone()
        else {
            return None;
        };
        let Expr::Try { value, span } = self.tree.expr(init).clone() else {
            return None;
        };
        Some(GuardedLet {
            name: self.interner.resolve(name).to_owned(),
            name_span,
            mutable,
            annotation: ty,
            value,
            span,
        })
    }

    /// Builds one `try`'s split: the failure branch running the handlers, the
    /// success branch binding the payload and running the rest of the body.
    fn lower_try(
        &mut self,
        ctx: &mut FnCtx,
        guarded: &GuardedLet,
        rest: &[StmtId],
        guard: &mut Guard<'_>,
        out: &mut Vec<HirStmtId>,
    ) {
        guard.saw_try = true;
        let operand_span = self.tree.expr(guarded.value).span();
        let result = self.analyze_expr(ctx, guarded.value);
        let result_ty = self.program.expr(result).type_of();

        let Some(shape) = self.result_shape(result_ty, operand_span) else {
            // The operand is not `Result`-shaped, so there is no payload to
            // bind and no failure to route. The rest of the body is still
            // analyzed, with the binding declared as `Type::Error`, so its own
            // mistakes surface instead of an avalanche of unknown-name reports.
            ctx.push_scope();
            let local = ctx.declare(&guarded.name, Type::Error, guarded.mutable);
            ctx.note_binding_span(local, guarded.name_span);
            let tail = self.lower_guarded(ctx, rest, guard);
            ctx.pop_scope();
            out.extend(tail);
            return;
        };

        if !self.agree_on_failure(guard, shape.failure, guarded.span) {
            return;
        }

        // Hidden, so nothing in either branch can name or shadow the result's
        // storage. It is read once for its tag and once for whichever payload
        // the branch it lands in projects.
        let slot = ctx.declare_hidden(result_ty, false);
        let bind = self.program.stmts.alloc(HirStmt::Let {
            local: slot,
            init: result,
        });
        out.push(bind);

        let tag_slot = self.bind_tag(ctx, slot, result_ty, out);
        let frame = TryFrame {
            slot,
            result_ty,
            shape,
        };

        let then_body = self.lower_handlers(ctx, &frame, guard);
        let else_body = self.lower_success(ctx, &frame, guarded, rest, guard);
        let cond = self.tag_test(tag_slot, frame.shape.error_tag);
        let hir = self.program.stmts.alloc(HirStmt::If {
            cond,
            then_body,
            else_body,
        });
        out.push(hir);
    }

    /// Binds the `Ok` payload to the name the `let` wrote, then lowers the rest
    /// of the body beneath it.
    fn lower_success(
        &mut self,
        ctx: &mut FnCtx,
        frame: &TryFrame,
        guarded: &GuardedLet,
        rest: &[StmtId],
        guard: &mut Guard<'_>,
    ) -> Vec<HirStmtId> {
        ctx.push_scope();
        let read = self.program.exprs.alloc(HirExpr::Local {
            local: frame.slot,
            ty: frame.result_ty,
        });
        let payload = self.program.exprs.alloc(HirExpr::EnumPayload {
            value: read,
            ty: frame.shape.ok_payload,
        });

        // An annotation is checked exactly as a plain `let`'s is: the reference
        // never writes one on a `try`, but the syntax admits it and silently
        // ignoring one would be worse than checking it.
        let local_ty = match guarded.annotation {
            Some(type_ref) => {
                let declared = self.resolve_type_ref(type_ref);
                if !frame.shape.ok_payload.assignable_to(declared) {
                    self.emit(
                        guarded.name_span,
                        "KSEM020",
                        format!(
                            "binding annotated `{}` cannot hold a value of type `{}`",
                            self.type_name(declared),
                            self.type_name(frame.shape.ok_payload)
                        ),
                    );
                }
                declared
            }
            None => frame.shape.ok_payload,
        };

        let local = ctx.declare(&guarded.name, local_ty, guarded.mutable);
        ctx.note_binding_span(local, guarded.name_span);
        let bind = self.program.stmts.alloc(HirStmt::Let {
            local,
            init: payload,
        });
        let mut body = vec![bind];
        body.extend(self.lower_guarded(ctx, rest, guard));
        ctx.pop_scope();
        body
    }

    /// Projects the failure out of the result and runs the handler chain over
    /// its tag.
    fn lower_handlers(
        &mut self,
        ctx: &mut FnCtx,
        frame: &TryFrame,
        guard: &mut Guard<'_>,
    ) -> Vec<HirStmtId> {
        ctx.push_scope();
        let read = self.program.exprs.alloc(HirExpr::Local {
            local: frame.slot,
            ty: frame.result_ty,
        });
        let failure_ty = Type::Enum(frame.shape.failure);
        let payload = self.program.exprs.alloc(HirExpr::EnumPayload {
            value: read,
            ty: failure_ty,
        });
        let failure_slot = ctx.declare_hidden(failure_ty, false);
        let bind = self.program.stmts.alloc(HirStmt::Let {
            local: failure_slot,
            init: payload,
        });
        let mut body = vec![bind];

        let tag_slot = self.bind_tag(ctx, failure_slot, failure_ty, &mut body);
        let resolved =
            self.resolve_handlers(ctx, frame.shape.failure, failure_slot, guard.handlers);
        self.check_handler_coverage(frame.shape.failure, &resolved, guard);
        body.extend(self.build_chain(tag_slot, resolved));
        ctx.pop_scope();
        body
    }

    /// Reads an enum value's tag into a hidden `Int` slot, so a chain of arms
    /// asks one question of one value instead of cloning it once per arm.
    fn bind_tag(
        &mut self,
        ctx: &mut FnCtx,
        slot: LocalId,
        ty: Type,
        out: &mut Vec<HirStmtId>,
    ) -> LocalId {
        let read = self.program.exprs.alloc(HirExpr::Local { local: slot, ty });
        let tag = self.program.exprs.alloc(HirExpr::EnumTag { value: read });
        let tag_slot = ctx.declare_hidden(Type::INT, false);
        let bind = self.program.stmts.alloc(HirStmt::Let {
            local: tag_slot,
            init: tag,
        });
        out.push(bind);
        tag_slot
    }

    /// Resolves each handler arm against the failure enum, reporting unknown
    /// and repeated variants in source order.
    fn resolve_handlers(
        &mut self,
        ctx: &mut FnCtx,
        failure: EnumId,
        slot: LocalId,
        handlers: &[MatchArm],
    ) -> Vec<ResolvedArm> {
        let mut resolved: Vec<ResolvedArm> = Vec::with_capacity(handlers.len());
        for arm in handlers {
            let name = self.interner.resolve(arm.variant).to_owned();
            let Some(tag) = self
                .program
                .types
                .enums()
                .get(failure)
                .and_then(|def| def.variant_index(&name))
            else {
                self.emit(
                    arm.variant_span,
                    "KSEM140",
                    format!(
                        "`{name}` is not a variant of the failure enum `{}`",
                        self.type_name(Type::Enum(failure))
                    ),
                );
                continue;
            };
            if resolved.iter().any(|existing| existing.tag == tag) {
                self.emit(
                    arm.variant_span,
                    "KSEM142",
                    format!("failure `{name}` is already handled by an earlier arm"),
                );
                continue;
            }
            let body = self.analyze_arm_body(ctx, failure, slot, tag, arm);
            resolved.push(ResolvedArm { tag, body });
        }
        resolved
    }

    /// Reports failure variants no handler covers.
    ///
    /// Skipped when an arm failed to resolve, for the reason `match`'s coverage
    /// check skips: a misspelled variant already reported would otherwise be
    /// told a second time as the variant it failed to name being uncovered.
    fn check_handler_coverage(
        &mut self,
        failure: EnumId,
        resolved: &[ResolvedArm],
        guard: &Guard<'_>,
    ) {
        if resolved.len() != guard.handlers.len() {
            return;
        }
        let Some(def) = self.program.types.enums().get(failure) else {
            return;
        };
        let missing: Vec<String> = def
            .variants
            .iter()
            .enumerate()
            .filter(|(index, _)| !resolved.iter().any(|arm| arm.tag as usize == *index))
            .map(|(_, variant)| variant.name.clone())
            .collect();
        if missing.is_empty() {
            return;
        }
        let name = self.type_name(Type::Enum(failure));
        self.emit(
            guard.attempt_span,
            "KSEM139",
            format!(
                "`handle` does not cover every failure of `{name}`; missing {}",
                missing.join(", ")
            ),
        );
    }

    /// Checks a `try` against the failure enum the earlier ones settled on.
    ///
    /// Returns `false` when they disagree, which drops this `try` rather than
    /// building a chain of handlers that belong to a different enum.
    fn agree_on_failure(&mut self, guard: &mut Guard<'_>, failure: EnumId, span: Span) -> bool {
        match guard.agreed {
            Some((agreed, _)) if *agreed != failure => {
                let first = self.type_name(Type::Enum(*agreed));
                let second = self.type_name(Type::Enum(failure));
                self.emit(
                    span,
                    "KSEM141",
                    format!(
                        "every `try` in one `attempt` must fail with the same enum; \
                         an earlier `try` fails with `{first}`, this one with `{second}`"
                    ),
                );
                false
            }
            Some(_) => true,
            None => {
                *guard.agreed = Some((failure, span));
                true
            }
        }
    }

    /// Reads the `Ok`/`Error` shape off a `try` operand's type.
    ///
    /// "`Result`-shaped" is structural, not nominal: any enum with an `Ok`
    /// variant and an `Error` variant whose payload is itself an enum will do.
    /// The reference's own failing tests declare a local
    /// `enum Outcome { Ok: Int  Error: AppError }` and `try` it, so requiring
    /// one particular declared type would reject a program it accepts.
    fn result_shape(&mut self, ty: Type, span: Span) -> Option<ResultShape> {
        // `Type::Error` is already reported; a second diagnostic here would
        // only bury the first.
        if ty == Type::Error {
            return None;
        }
        let Type::Enum(enum_id) = ty else {
            let found = self.type_name(ty);
            return self.refuse_try_operand(span, &format!("this is `{found}`"));
        };
        // An id the table does not know is already a reported failure; a second
        // diagnostic here would bury the first.
        let def = self.program.types.enums().get(enum_id)?;
        let name = def.name.clone();
        let (Some(ok_tag), Some(error_tag)) = (def.variant_index("Ok"), def.variant_index("Error"))
        else {
            return self
                .refuse_try_operand(span, &format!("`{name}` has no `Ok` and `Error` pair"));
        };
        let ok_payload = def.variant(ok_tag).and_then(|variant| variant.payload);
        let error_payload = def.variant(error_tag).and_then(|variant| variant.payload);

        let Some(ok_payload) = ok_payload else {
            return self
                .refuse_try_operand(span, &format!("`{name}.Ok` carries no value to unwrap"));
        };
        let Some(Type::Enum(failure)) = error_payload else {
            return self
                .refuse_try_operand(span, &format!("`{name}.Error` carries no failure enum"));
        };
        Some(ResultShape {
            error_tag,
            failure,
            ok_payload,
        })
    }

    /// Reports a `try` on something that is not `Result`-shaped.
    fn refuse_try_operand(&mut self, span: Span, reason: &str) -> Option<ResultShape> {
        self.emit(
            span,
            "KSEM138",
            format!(
                "`try` needs a `Result`-shaped value — an enum with an `Ok` variant and an \
                 `Error` variant carrying a failure enum — but {reason}"
            ),
        );
        None
    }
}

/// A recognized `let <name> = try <expr>`, taken apart for lowering.
struct GuardedLet {
    /// The name the `Ok` payload binds to.
    name: String,
    /// Span of that name, for an annotation mismatch.
    name_span: Span,
    /// Whether it was written `var`.
    mutable: bool,
    /// The type annotation, when one was written.
    annotation: Option<TypeRefId>,
    /// The `Result`-shaped operand of the `try`.
    value: ExprId,
    /// Span covering `try <value>`, for the failure-agreement report.
    span: Span,
}
