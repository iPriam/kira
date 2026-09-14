//! The `match` desugar: an enum's variants or a scalar's values into the
//! `if`/`else` chain the HIR already has.
//!
//! # What this costs the backends
//!
//! Nothing. A `match` becomes a chain of [`HirStmt::If`] over a `Bool` test —
//! an `Int` tag comparison for an enum, an equality or a range test for a
//! scalar — so the IR, the bytecode compiler, the VM, the LLVM backend, and the
//! WASM backend never learn `match` exists. The one thing an enum `match` needs
//! that a plain `if` did not is a way to read a variant's payload, and that is a
//! single expression node ([`HirExpr::EnumPayload`]) rather than a statement
//! form.
//!
//! Given `match e { A -> P  B(x) -> Q }`:
//!
//! ```text
//! let <subject> = e              // hidden: evaluated once
//! let <tag>     = EnumTag(<subject>)
//! if <tag> == 0 { P }
//! else           { let x = EnumPayload(<subject>); Q }
//! ```
//!
//! A scalar `match n { 1, 2 -> P  0..10 -> Q  else -> R }` desugars the same
//! way over the subject directly:
//!
//! ```text
//! let <subject> = n
//! if <subject> == 1 || <subject> == 2 { P }
//! else if <subject> >= 0 && <subject> < 10 { Q }
//! else { R }
//! ```
//!
//! # Why the last arm is the `else`, not another `if`
//!
//! Because a `match` is checked exhaustive, the last arm runs whenever no
//! earlier one did — so making it the unconditional tail is not an
//! optimization, it is the truth. An `else` arm is written last for the same
//! reason, and an arm after it can never run, which is refused.

mod scalar;
pub(crate) use scalar::ScalarCoverage;

use kira_semantics_model::hir::{HirBinaryOp, HirExpr, HirExprId, HirStmt, HirStmtId, LocalId};
use kira_semantics_model::{EnumId, Type};
use kira_source::Span;
use kira_syntax_model::ast::{Block, ExprId, MatchArm, MatchBinding, MatchPattern};

use crate::analyze::{Analyzer, FnCtx};

/// One arm, resolved to the test that selects it and the body it runs.
pub(crate) struct ResolvedArm {
    /// The `Bool` condition selecting this arm, or `None` for the `else` arm
    /// and the unconditional last arm of an exhaustive chain.
    pub(crate) test: Option<HirExprId>,
    /// The statements the arm runs, payload binding included.
    pub(crate) body: Vec<HirStmtId>,
}

#[derive(Clone, Copy)]
pub(crate) struct EnumSubject {
    pub(crate) id: EnumId,
    pub(crate) slot: LocalId,
}

pub(crate) struct EnumArmResolution {
    pub(crate) test: Option<HirExprId>,
    pub(crate) binding: Option<(MatchBinding, u32)>,
}

impl Analyzer<'_> {
    /// Analyzes a `match`, appending the chain it desugars to onto `out`.
    pub(crate) fn analyze_match(
        &mut self,
        ctx: &mut FnCtx,
        subject: ExprId,
        arms: &[MatchArm],
        span: Span,
        out: &mut Vec<HirStmtId>,
    ) {
        let subject_span = self.tree.expr(subject).span();
        let subject_expr = self.analyze_expr(ctx, subject);
        let subject_ty = self.program.expr(subject_expr).type_of();

        if let Type::Enum(enum_id) = subject_ty {
            let slot = self.bind_hidden(ctx, subject_ty, subject_expr, out);
            self.analyze_enum_match(ctx, EnumSubject { id: enum_id, slot }, arms, span, out);
            return;
        }
        if self.is_scalar_match_subject(subject_ty) {
            let slot = self.bind_hidden(ctx, subject_ty, subject_expr, out);
            self.analyze_scalar_match(ctx, slot, subject_ty, arms, span, out);
            return;
        }

        // A `match` selects on a variant or a scalar value, so a subject that is
        // neither is refused rather than guessed at. `Type::Error` is already
        // reported, so it passes silently. The arms' bodies are still analyzed
        // so their own mistakes surface.
        if subject_ty != Type::Error {
            self.emit(
                subject_span,
                "KSEM125",
                format!(
                    "a `match` subject must be an enum, an `Int`, a `Bool`, or a `String`, \
                     found `{}`",
                    self.type_name(subject_ty)
                ),
            );
        }
        for arm in arms {
            ctx.push_scope();
            for pattern in &arm.patterns {
                if let MatchPattern::Variant {
                    binding: Some(binding),
                    ..
                } = pattern
                {
                    let name = self.interner.resolve(binding.name).to_owned();
                    let local = ctx.declare(&name, Type::Error, false);
                    ctx.note_binding_span(local, binding.span);
                }
            }
            let body = self.analyze_block(ctx, &arm.body);
            ctx.pop_scope();
            out.extend(body);
        }
    }

    /// Binds `value` to a fresh hidden slot and returns it, so a subject is read
    /// once per test and once per payload projection rather than re-evaluated.
    fn bind_hidden(
        &mut self,
        ctx: &mut FnCtx,
        ty: Type,
        value: HirExprId,
        out: &mut Vec<HirStmtId>,
    ) -> LocalId {
        let slot = ctx.declare_hidden(ty, false);
        let bind = self.program.stmts.alloc(HirStmt::Let {
            local: slot,
            init: value,
        });
        out.push(bind);
        slot
    }

    /// Whether a subject type admits a scalar `match`: an integer of any
    /// spelling, a `Bool`, or a `String`.
    pub(crate) fn is_scalar_match_subject(&self, ty: Type) -> bool {
        matches!(ty, Type::Int(_) | Type::Bool | Type::String)
    }

    // --- enum -----------------------------------------------------------------

    /// Analyzes a `match` over an enum subject.
    fn analyze_enum_match(
        &mut self,
        ctx: &mut FnCtx,
        subject: EnumSubject,
        arms: &[MatchArm],
        span: Span,
        out: &mut Vec<HirStmtId>,
    ) {
        let EnumSubject { id: enum_id, slot } = subject;
        let read = self.program.exprs.alloc(HirExpr::Local {
            local: slot,
            ty: Type::Enum(enum_id),
        });
        let tag_expr = self.program.exprs.alloc(HirExpr::EnumTag { value: read });
        let tag_slot = ctx.declare_hidden(Type::INT, false);
        let bind_tag = self.program.stmts.alloc(HirStmt::Let {
            local: tag_slot,
            init: tag_expr,
        });
        out.push(bind_tag);

        let mut branch = crate::ownership::BranchMoves::start(ctx);
        let mut resolved: Vec<ResolvedArm> = Vec::with_capacity(arms.len());
        let mut covered: Vec<u32> = Vec::new();
        let mut wildcard = false;
        let mut exhaustive_by_arms = true;

        for arm in arms {
            if wildcard {
                self.emit(
                    arm.span,
                    "KSEM381",
                    "an `else` arm must be the last arm; the arms after it can never run",
                );
                exhaustive_by_arms = false;
                continue;
            }
            if arm_is_wildcard(arm) {
                wildcard = true;
                branch.enter_arm(ctx);
                let body = self.analyze_block(ctx, &arm.body);
                branch.leave_arm(ctx, self.body_definitely_returns(&body));
                resolved.push(ResolvedArm { test: None, body });
                continue;
            }
            let Some(resolution) = self.resolve_enum_arm(enum_id, tag_slot, arm, &mut covered)
            else {
                exhaustive_by_arms = false;
                continue;
            };
            let EnumArmResolution { test, binding } = resolution;
            branch.enter_arm(ctx);
            let body = self.analyze_enum_arm_body(ctx, enum_id, slot, binding, &arm.body);
            branch.leave_arm(ctx, self.body_definitely_returns(&body));
            resolved.push(ResolvedArm { test, body });
        }

        let exhaustive =
            self.check_enum_coverage(enum_id, &covered, wildcard, exhaustive_by_arms, span);
        branch.finish(ctx, exhaustive);
        out.extend(self.build_chain(resolved));
    }

    /// Resolves one non-wildcard enum arm to its selecting test and, when it is
    /// a single variant carrying a payload, its binding. Reports unknown and
    /// repeated variants, alternation with a binding, and returns `None` when
    /// the arm named no resolvable variant.
    pub(crate) fn resolve_enum_arm(
        &mut self,
        enum_id: EnumId,
        tag_slot: LocalId,
        arm: &MatchArm,
        covered: &mut Vec<u32>,
    ) -> Option<EnumArmResolution> {
        let mut tags: Vec<u32> = Vec::new();
        let mut bindings: Vec<(MatchBinding, u32)> = Vec::new();
        let alternation = arm.patterns.len() > 1;
        for pattern in &arm.patterns {
            let MatchPattern::Variant {
                variant,
                variant_span,
                binding: pattern_binding,
            } = pattern
            else {
                self.emit(
                    pattern.span(),
                    "KSEM383",
                    format!(
                        "a `match` on an enum takes variant patterns, not `{}`",
                        self.type_name(Type::Enum(enum_id))
                    ),
                );
                continue;
            };
            let name = self.interner.resolve(*variant).to_owned();
            let Some(tag) = self
                .program
                .types
                .enums()
                .get(enum_id)
                .and_then(|def| def.variant_index(&name))
            else {
                self.emit(
                    *variant_span,
                    "KSEM126",
                    format!(
                        "enum `{}` has no variant `{name}`",
                        self.type_name(Type::Enum(enum_id))
                    ),
                );
                continue;
            };
            if covered.contains(&tag) || tags.contains(&tag) {
                self.emit(
                    *variant_span,
                    "KSEM127",
                    format!("variant `{name}` is already matched by an earlier arm"),
                );
                continue;
            }
            if let Some(pattern_binding) = pattern_binding {
                bindings.push((*pattern_binding, tag));
            }
            tags.push(tag);
        }
        if tags.is_empty() {
            return None;
        }
        // A binding on an alternation is refused once, but the first is still
        // declared so the body's use of it does not cascade into an
        // undefined-name error on top of the real one.
        if alternation && !bindings.is_empty() {
            self.emit(
                arm.span,
                "KSEM384",
                "a payload binding is not allowed on an arm that matches several patterns",
            );
        }
        let binding = bindings.first().copied();
        covered.extend(tags.iter().copied());
        let tests: Vec<HirExprId> = tags
            .iter()
            .map(|&tag| self.tag_test(tag_slot, tag))
            .collect();
        let test = self.any_of(tests);
        Some(EnumArmResolution { test, binding })
    }

    /// Analyzes one enum arm's body, with its payload binding in scope.
    pub(crate) fn analyze_enum_arm_body(
        &mut self,
        ctx: &mut FnCtx,
        enum_id: EnumId,
        slot: LocalId,
        binding: Option<(MatchBinding, u32)>,
        body: &Block,
    ) -> Vec<HirStmtId> {
        ctx.push_scope();
        let mut out = Vec::new();
        if let Some((binding, tag)) = binding {
            // The binding names one variant's payload; a single-variant arm is
            // the only shape that reaches here with a binding, so exactly one
            // tag's payload type is in play.
            self.bind_enum_payload(ctx, enum_id, slot, binding, tag, &mut out);
        }
        out.extend(self.analyze_block(ctx, body));
        ctx.pop_scope();
        out
    }

    /// Declares an arm's payload binding, reading it out of the subject slot.
    pub(crate) fn bind_enum_payload(
        &mut self,
        ctx: &mut FnCtx,
        enum_id: EnumId,
        slot: LocalId,
        binding: MatchBinding,
        tag: u32,
        out: &mut Vec<HirStmtId>,
    ) {
        // Its payload type is read from the enum definition. A binding on a
        // payload-less variant simply binds nothing after reporting.
        let payload_ty = self
            .program
            .types
            .enums()
            .get(enum_id)
            .and_then(|def| def.variant(tag))
            .and_then(|variant| variant.payload);
        let Some(ty) = payload_ty else {
            self.emit(
                binding.span,
                "KSEM128",
                "this variant carries no payload to bind",
            );
            return;
        };
        let read = self.program.exprs.alloc(HirExpr::Local {
            local: slot,
            ty: Type::Enum(enum_id),
        });
        let payload = self
            .program
            .exprs
            .alloc(HirExpr::EnumPayload { value: read, ty });
        let name = self.interner.resolve(binding.name).to_owned();
        let local = ctx.declare(&name, ty, false);
        ctx.note_binding_span(local, binding.span);
        let stmt = self.program.stmts.alloc(HirStmt::Let {
            local,
            init: payload,
        });
        out.push(stmt);
    }

    /// Reports variants no arm covers, unless an `else` arm covers them.
    /// Returns whether the arms are exhaustive.
    pub(crate) fn check_enum_coverage(
        &mut self,
        enum_id: EnumId,
        covered: &[u32],
        wildcard: bool,
        clean: bool,
        span: Span,
    ) -> bool {
        if !clean {
            return false;
        }
        if wildcard {
            return true;
        }
        let Some(def) = self.program.types.enums().get(enum_id) else {
            return false;
        };
        let missing: Vec<String> = def
            .variants
            .iter()
            .enumerate()
            .filter(|(index, _)| !covered.contains(&(*index as u32)))
            .map(|(_, variant)| variant.name.clone())
            .collect();
        if missing.is_empty() {
            return true;
        }
        self.emit(
            span,
            "KSEM129",
            format!(
                "`match` does not cover every variant of `{}`; missing {}",
                self.type_name(Type::Enum(enum_id)),
                missing.join(", ")
            ),
        );
        false
    }

    // --- shared ---------------------------------------------------------------

    /// Combines several `Bool` tests with `||`, or returns the single test when
    /// there is one. Empty input never happens: a resolved arm has at least one
    /// test.
    pub(crate) fn any_of(&mut self, tests: Vec<HirExprId>) -> Option<HirExprId> {
        let mut iter = tests.into_iter();
        let mut combined = iter.next()?;
        for test in iter {
            combined = self.program.exprs.alloc(HirExpr::Binary {
                op: HirBinaryOp::Or,
                lhs: combined,
                rhs: test,
                ty: Type::Bool,
            });
        }
        Some(combined)
    }

    /// Assembles the resolved arms into the `if`/`else` chain.
    ///
    /// Built from the back, because an `else` has to exist before the `if` that
    /// points at it. The last arm becomes the chain's tail unconditionally —
    /// see this module's header for why that is correctness, not shortcut.
    pub(crate) fn build_chain(&mut self, resolved: Vec<ResolvedArm>) -> Vec<HirStmtId> {
        let mut arms = resolved.into_iter().rev();
        let Some(last) = arms.next() else {
            return Vec::new();
        };
        let mut chain = last.body;
        for arm in arms {
            if let Some(cond) = arm.test {
                let hir = self.program.stmts.alloc(HirStmt::If {
                    cond,
                    then_body: arm.body,
                    else_body: chain,
                });
                chain = vec![hir];
            } else {
                chain = arm.body;
            }
        }
        chain
    }

    /// Builds `<tag> == <discriminant>` for one enum arm.
    pub(crate) fn tag_test(&mut self, tag_slot: LocalId, tag: u32) -> HirExprId {
        let read = self.program.exprs.alloc(HirExpr::Local {
            local: tag_slot,
            ty: Type::INT,
        });
        let expected = self.program.exprs.alloc(HirExpr::Int(i64::from(tag)));
        self.program.exprs.alloc(HirExpr::Binary {
            op: HirBinaryOp::EqInt,
            lhs: read,
            rhs: expected,
            ty: Type::Bool,
        })
    }
}
/// Whether an arm is the `else` catch-all.
pub(crate) fn arm_is_wildcard(arm: &MatchArm) -> bool {
    arm.patterns
        .iter()
        .any(|pattern| matches!(pattern, MatchPattern::Wildcard { .. }))
}
