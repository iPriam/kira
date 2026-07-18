//! The `match` desugar: an enum's variants into the `if`/`else` chain the HIR
//! already has.
//!
//! # What this costs the backends
//!
//! Nothing. A `match` becomes exactly what a `switch` becomes — a chain of
//! [`HirStmt::If`] over an `Int` comparison — so the IR, the bytecode compiler,
//! the VM, the LLVM backend, and the WASM backend never learn `match` exists.
//! The one thing `match` needs that `switch` did not is a way to read a
//! variant's payload, and that is a single expression node
//! ([`HirExpr::EnumPayload`]) rather than a statement form.
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
//! # Why the last arm is the `else`, not another `if`
//!
//! Because a `match` is checked exhaustive, the last arm runs whenever no
//! earlier one did — so making it the unconditional `else` is not an
//! optimization, it is the truth. It is also what makes
//!
//! ```text
//! function areaOf(c: Circle) -> Int {
//!     match c { Filled(shape) -> return shape.area; Empty -> return 0; }
//! }
//! ```
//!
//! a function that definitely returns: `body_definitely_returns` requires
//! *both* arms of an `if` to return, and a trailing empty `else` would never
//! satisfy it. The corpus writes that function with no trailing `return`, so
//! the shape is forced.
//!
//! # Why `match` is checked and `switch` is not
//!
//! A `switch` label is an arbitrary expression, so there is no set of labels to
//! be exhaustive over and no notion of two labels being the same one. A `match`
//! arm names a variant of a known enum, so both questions have answers — and
//! the corpus expects them asked. Neither check belongs to the other construct.

use kira_semantics_model::hir::{HirExpr, HirExprId, HirStmt, HirStmtId, LocalId};
use kira_semantics_model::{EnumId, Type};
use kira_source::Span;
use kira_syntax_model::ast::{ExprId, MatchArm};

use crate::analyze::{Analyzer, FnCtx};

/// One arm, resolved against the subject's enum.
pub(crate) struct ResolvedArm {
    /// The variant discriminant.
    pub(crate) tag: u32,
    /// The statements the arm runs, payload binding included.
    pub(crate) body: Vec<HirStmtId>,
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

        // A `match` selects on a variant, so a subject with no variants to
        // select on is refused rather than guessed at. `Type::Error` is already
        // reported, so it passes silently.
        let Type::Enum(enum_id) = subject_ty else {
            if subject_ty != Type::Error {
                self.emit(
                    subject_span,
                    "KSEM125",
                    format!(
                        "a `match` subject must be an enum, found `{}`",
                        self.type_name(subject_ty)
                    ),
                );
            }
            // Still analyze the arms' bodies so their own mistakes surface —
            // but declare each binding first, as `Type::Error`. Without that,
            // one bad subject turns every use of every binding into a second
            // "undefined name" diagnostic, burying the error that caused it.
            for arm in arms {
                ctx.push_scope();
                if let Some(binding) = arm.binding {
                    let name = self.interner.resolve(binding.name).to_owned();
                    ctx.declare(&name, Type::Error, false);
                }
                let body = self.analyze_block(ctx, &arm.body);
                ctx.pop_scope();
                out.extend(body);
            }
            return;
        };

        // Hidden, so no arm can name or shadow the subject's storage. It is
        // read once per payload projection and once for the tag; every read
        // clones and every consumer drops its clone, so the slot itself is
        // freed exactly once, when the frame is.
        let slot = ctx.declare_hidden(subject_ty, false);
        let bind = self.program.stmts.alloc(HirStmt::Let {
            local: slot,
            init: subject_expr,
        });
        out.push(bind);

        // The tag is read once rather than per arm: a chain of N arms would
        // otherwise clone and drop the enum N times to ask N questions about
        // one value.
        let read = self.program.exprs.alloc(HirExpr::Local {
            local: slot,
            ty: subject_ty,
        });
        let tag_expr = self.program.exprs.alloc(HirExpr::EnumTag { value: read });
        let tag_slot = ctx.declare_hidden(Type::INT, false);
        let bind_tag = self.program.stmts.alloc(HirStmt::Let {
            local: tag_slot,
            init: tag_expr,
        });
        out.push(bind_tag);

        let resolved = self.resolve_arms(ctx, enum_id, slot, arms);
        self.check_coverage(enum_id, &resolved, arms, span);
        out.extend(self.build_chain(tag_slot, resolved));
    }

    /// Resolves each arm in source order, reporting unknown variants and
    /// duplicated ones as it goes.
    ///
    /// Source order matters: it is the order the diagnostics come out in, and
    /// it is what makes the *second* mention of a variant the duplicate.
    fn resolve_arms(
        &mut self,
        ctx: &mut FnCtx,
        enum_id: EnumId,
        slot: LocalId,
        arms: &[MatchArm],
    ) -> Vec<ResolvedArm> {
        let mut resolved: Vec<ResolvedArm> = Vec::with_capacity(arms.len());
        for arm in arms {
            let name = self.interner.resolve(arm.variant).to_owned();
            let Some(tag) = self
                .program
                .types
                .enums()
                .get(enum_id)
                .and_then(|def| def.variant_index(&name))
            else {
                self.emit(
                    arm.variant_span,
                    "KSEM126",
                    format!(
                        "enum `{}` has no variant `{name}`",
                        self.type_name(Type::Enum(enum_id))
                    ),
                );
                // Skipped rather than kept: an arm with no tag has no test to
                // build, and counting it toward coverage would turn one
                // mistake into a spurious second diagnostic.
                continue;
            };
            if resolved.iter().any(|existing| existing.tag == tag) {
                self.emit(
                    arm.variant_span,
                    "KSEM127",
                    format!("variant `{name}` is already matched by an earlier arm"),
                );
                continue;
            }
            let body = self.analyze_arm_body(ctx, enum_id, slot, tag, arm);
            resolved.push(ResolvedArm { tag, body });
        }
        resolved
    }

    /// Analyzes one arm's body, with its payload binding in scope.
    ///
    /// The binding is declared in a scope of its own, wrapping the body's, so
    /// it is visible to the arm and to nothing else — two arms may bind the
    /// same name to different variants' payloads without colliding.
    pub(crate) fn analyze_arm_body(
        &mut self,
        ctx: &mut FnCtx,
        enum_id: EnumId,
        slot: LocalId,
        tag: u32,
        arm: &MatchArm,
    ) -> Vec<HirStmtId> {
        let payload_ty = self
            .program
            .types
            .enums()
            .get(enum_id)
            .and_then(|def| def.variant(tag))
            .and_then(|variant| variant.payload);

        ctx.push_scope();
        let mut body = Vec::new();
        match (arm.binding, payload_ty) {
            (Some(binding), Some(ty)) => {
                let read = self.program.exprs.alloc(HirExpr::Local {
                    local: slot,
                    ty: Type::Enum(enum_id),
                });
                let payload = self
                    .program
                    .exprs
                    .alloc(HirExpr::EnumPayload { value: read, ty });
                let name = self.interner.resolve(binding.name).to_owned();
                // Immutable: the corpus only ever reads a bound payload, and a
                // binding that could be written would raise a question the
                // corpus does not answer — whether the write reaches the enum.
                let local = ctx.declare(&name, ty, false);
                let stmt = self.program.stmts.alloc(HirStmt::Let {
                    local,
                    init: payload,
                });
                body.push(stmt);
            }
            (Some(binding), None) => {
                let name = self.interner.resolve(arm.variant).to_owned();
                self.emit(
                    binding.span,
                    "KSEM128",
                    format!("variant `{name}` carries no payload to bind"),
                );
            }
            (None, Some(_)) => {
                // Legal: an arm may ignore a payload it does not need. The
                // corpus writes `Empty -> return 0` beside `Filled(s) -> …`,
                // and nothing requires the payload be named.
            }
            (None, None) => {}
        }
        body.extend(self.analyze_block(ctx, &arm.body));
        ctx.pop_scope();
        body
    }

    /// Reports variants no arm covers.
    ///
    /// Skipped when an arm failed to resolve: a misspelled variant already
    /// reported would otherwise also show up as the variant it failed to name
    /// being uncovered, which is one mistake told twice.
    fn check_coverage(
        &mut self,
        enum_id: EnumId,
        resolved: &[ResolvedArm],
        arms: &[MatchArm],
        span: Span,
    ) {
        if resolved.len() != arms.len() {
            return;
        }
        let Some(def) = self.program.types.enums().get(enum_id) else {
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
        self.emit(
            span,
            "KSEM129",
            format!(
                "`match` does not cover every variant of `{}`; missing {}",
                self.type_name(Type::Enum(enum_id)),
                missing.join(", ")
            ),
        );
    }

    /// Assembles the resolved arms into the `if`/`else` chain.
    ///
    /// Built from the back, because an `else` has to exist before the `if` that
    /// points at it. The last arm becomes the chain's tail unconditionally —
    /// see this module's header for why that is correctness, not shortcut.
    pub(crate) fn build_chain(
        &mut self,
        tag_slot: LocalId,
        resolved: Vec<ResolvedArm>,
    ) -> Vec<HirStmtId> {
        let mut arms = resolved.into_iter().rev();
        let Some(last) = arms.next() else {
            return Vec::new();
        };
        let mut chain = last.body;
        for arm in arms {
            let cond = self.tag_test(tag_slot, arm.tag);
            let hir = self.program.stmts.alloc(HirStmt::If {
                cond,
                then_body: arm.body,
                else_body: chain,
            });
            chain = vec![hir];
        }
        chain
    }

    /// Builds `<tag> == <discriminant>` for one arm.
    pub(crate) fn tag_test(&mut self, tag_slot: LocalId, tag: u32) -> HirExprId {
        let read = self.program.exprs.alloc(HirExpr::Local {
            local: tag_slot,
            ty: Type::INT,
        });
        let expected = self.program.exprs.alloc(HirExpr::Int(i64::from(tag)));
        self.program.exprs.alloc(HirExpr::Binary {
            op: kira_semantics_model::hir::HirBinaryOp::EqInt,
            lhs: read,
            rhs: expected,
            ty: Type::Bool,
        })
    }
}
