//! The compiler-known `Drop` trait: a body the engines run where they already
//! release the value.
//!
//! A type claims `Drop` the way it claims any trait — in its declaration's
//! conformance list, or in an `extend T: Drop { … }` block — and implements the
//! one member the trait has:
//!
//! ```kira
//! extend Handle: Drop {
//!     function drop(borrow mut self) { closeHandle(raw) }
//! }
//! ```
//!
//! # What the claim buys and what it costs
//!
//! The body runs once, before the type's own members are released, at every
//! point either engine releases a value of the type. That is the whole of the
//! feature, and it is why the two costs below are not options:
//!
//! * **The body is never called by name.** `value.drop()` is `KSEM300`. A
//!   release is the compiler's to schedule; a hand-written call would run the
//!   body a second time and leave the value looking alive.
//! * **The type moves rather than copies.** A copy would be a second value with
//!   the same body to run, and the release that ran it once would run it twice.
//!   So a `Copyable` claim on a `Drop` type is refused with the same diagnostic
//!   that refuses one on a `String`-bearing struct.
//!
//! # How each engine finds the body
//!
//! [`kira_semantics_model::StructDef::drop_glue`] records the function, and the
//! type table travels to both backends unchanged. The native backend calls it
//! at the head of the type's release leaf; the VM records it on the heap object
//! at construction and runs it when the last holder goes, which is what makes
//! "exactly once" a runtime fact rather than a hope about where a value ended
//! up.

use kira_semantics_model::hir::{HirExpr, HirExprId};
use kira_semantics_model::{StructId, Type};
use kira_source::{SourceId, Span};
use kira_syntax_model::ast::{ConstructKind, Item};

use crate::analyze::Analyzer;

/// The one member the `Drop` trait declares.
pub(crate) const DROP_MEMBER: &str = "drop";

/// One read that took a value running a user `Drop` out of the value holding
/// it.
///
/// Recorded rather than reported on sight, because two positions read a member
/// without consuming it — the base of a further member read, and an argument a
/// callee borrows — and neither is known until the enclosing expression is
/// built. A read still on this list when the body finishes is one no such
/// position claimed.
#[derive(Debug, Clone, Copy)]
pub(crate) struct DropExtraction {
    /// The read.
    expr: HirExprId,
    /// The file it was written in, which the refusal is attributed to.
    source: SourceId,
    /// The member name's span.
    span: Span,
    /// The type the read produced.
    ty: Type,
}

impl Analyzer<'_> {
    /// Validates every `Drop` conformance and records the body each one names.
    ///
    /// Runs after signatures, because the body is a method and a method has no
    /// id until then, and before any body is analyzed, because whether a type
    /// runs a user drop decides whether it is released at all.
    pub(crate) fn record_user_drops(&mut self, callables: &[crate::analyze::Callable<'_>]) {
        let claims: Vec<(StructId, SourceId, Span)> = self
            .conformances
            .iter()
            .filter(|entry| entry.contract.trait_name() == Some(super::DROP))
            .map(|entry| (entry.ty, entry.source, entry.span))
            .collect();
        for (ty, source, span) in claims {
            self.source = source;
            let type_name = self.program.types.type_name(Type::Struct(ty));
            let qualified = format!("{type_name}.{DROP_MEMBER}");
            let Some(candidates) = self.sig_index.get(&qualified).cloned() else {
                self.emit(
                    span,
                    "KSEM301",
                    format!(
                        "`{type_name}` claims `Drop` but presents no `drop`: write \
                         `function drop(borrow mut self)` in its body or in an \
                         `extend {type_name}: Drop` block"
                    ),
                );
                continue;
            };
            // One `drop` and no other: the release site takes no arguments and
            // has nowhere to put a result, so a second overload would be a body
            // nothing could ever reach.
            let mut glue = None;
            for id in candidates {
                let (params, result, name_span, declared) = {
                    let sig = &self.sigs[id.0 as usize];
                    (sig.params.len(), sig.return_type, sig.name_span, sig.source)
                };
                if params == 1 && result == Type::Void {
                    glue = Some(id);
                    continue;
                }
                self.source = declared;
                self.emit(
                    name_span,
                    "KSEM301",
                    format!(
                        "`{type_name}.drop` must be written `function drop(borrow mut self)`: a \
                         release passes no arguments and has nowhere to put a result"
                    ),
                );
                self.source = source;
            }
            let Some(glue) = glue else {
                continue;
            };
            // A release happens wherever the value dies, and in a hybrid
            // program that is either engine — so the body is compiled into both
            // halves, which it can only be if it chose neither.
            if let Some(engine) = callables
                .get(glue.0 as usize)
                .and_then(|callable| callable.function.execution.annotation())
            {
                let name_span = self.sigs[glue.0 as usize].name_span;
                self.source = self.sigs[glue.0 as usize].source;
                self.emit(
                    name_span,
                    "KSEM301",
                    format!(
                        "`{type_name}.drop` may not declare `@{engine}`: a release happens \
                         wherever the value dies, so the body is compiled for every engine the \
                         program runs on rather than for one of them."
                    ),
                );
                self.source = source;
                continue;
            }
            self.program.types.structs_mut().set_drop_glue(ty, glue.0);
        }
    }

    /// Refuses a written call to a type's `drop`, reporting whether it did.
    ///
    /// Asked before a method call resolves, so the refusal names the rule
    /// rather than letting the call reach an ordinary body.
    pub(crate) fn refuse_direct_drop_call(
        &mut self,
        receiver: Type,
        name: &str,
        span: Span,
    ) -> bool {
        if name != DROP_MEMBER {
            return false;
        }
        let Type::Struct(id) = receiver else {
            return false;
        };
        if !self.conforms_to(id, super::DROP) {
            return false;
        }
        let type_name = self.program.types.type_name(receiver);
        self.emit(
            span,
            "KSEM300",
            format!(
                "`{type_name}.drop` is run by the release, not by a call: calling it here would \
                 run the body a second time and leave the value looking alive. Let the value go \
                 out of scope, or move it into something that owns it."
            ),
        );
        true
    }

    /// Refuses every enum variant payload that runs a user `Drop`.
    ///
    /// An enum is read by matching it, and a match binds the payload as a value
    /// of its own while the enum still holds it — the same second owner a
    /// member read is refused for, with no borrowed spelling to fall back on.
    /// The refusal is at the payload rather than at each match, because a
    /// payload nobody may bind is a payload nobody may read.
    ///
    /// Drains the recorded sites, so it may run more than once: a body that
    /// writes a generic instantiation mints an enum after the first pass.
    pub(crate) fn refuse_drop_enum_payloads(&mut self) {
        let here = self.source;
        for (ty, source, span) in std::mem::take(&mut self.enum_payload_sites) {
            if !self.program.types.runs_user_drop(ty) {
                continue;
            }
            self.source = source;
            let name = self.type_name(ty);
            self.emit(
                span,
                "KSEM306",
                format!(
                    "an enum payload may not be a `{name}`, which runs a user `Drop` body: \
                     matching the enum binds the payload as a value of its own while the enum \
                     still holds it, so the body would run twice for storage that only goes \
                     away once. Hold the value beside the enum, or give the payload a type that \
                     runs no body."
                ),
            );
        }
        self.source = here;
    }

    /// Refuses a construct-backed declaration that runs a user `Drop`.
    ///
    /// A family value *is* an enum whose payload is the declaration, and
    /// reading a member through the family projects that payload out — so the
    /// rule above applies to a declaration whose family nobody wrote by hand.
    /// The `Drop` may be claimed or inherited from a member; the question is
    /// what releasing the declaration runs, not how it came to run it.
    pub(crate) fn refuse_drop_construct_declarations(&mut self) {
        let backed: Vec<(SourceId, Span, StructId)> = self
            .tree
            .items_with_source()
            .filter_map(|(source, item)| match item {
                Item::Construct(declaration) => match declaration.kind {
                    ConstructKind::Backed { .. } => {
                        let name = self.interner.resolve(declaration.name);
                        let id = self
                            .program
                            .types
                            .structs()
                            .lookup_owned(self.imports.package_of(source), name)?;
                        Some((source, declaration.name_span, id))
                    }
                    ConstructKind::Family => None,
                },
                _ => None,
            })
            .collect();
        let here = self.source;
        for (source, span, id) in backed {
            let ty = Type::Struct(id);
            if !self.program.types.runs_user_drop(ty) {
                continue;
            }
            self.source = source;
            let name = self.type_name(ty);
            self.emit(
                span,
                "KSEM306",
                format!(
                    "`{name}` runs a user `Drop` body, which a construct-backed declaration may \
                     not: its family value carries the declaration as a payload, and reading a \
                     member through the family projects that payload out as a second value with \
                     the same body to run. Hold the value beside the declaration instead."
                ),
            );
        }
        self.source = here;
    }

    /// Records a member or element read whose value runs a user `Drop`.
    ///
    /// The value stays in the container the read came out of, so the read is a
    /// second value with the same body to run. Recorded rather than refused
    /// here: [`Analyzer::excuse_drop_extraction`] takes back the ones a
    /// non-consuming position claims.
    pub(crate) fn note_drop_extraction(&mut self, expr: HirExprId, span: Span) {
        let ty = self.program.expr(expr).type_of();
        if !self.program.types.runs_user_drop(ty) {
            return;
        }
        // A struct read out of a temporary is refused here rather than
        // recorded: no enclosing position can excuse it, because what an
        // excusal promises is that the container goes on holding the value, and
        // a temporary is released at the end of the statement that made it. A
        // struct is the one shape where that matters — an array or an enum read
        // hands back a share of one object, and releasing a share runs no body
        // while the original still holds it.
        if matches!(ty, Type::Struct(_)) && !self.reads_a_place(expr) {
            let name = self.type_name(ty);
            self.emit(
                span,
                "KSEM302",
                format!(
                    "this takes a `{name}` out of a temporary, and `{name}` runs a user `Drop` \
                     body: the value it was read from is released at the end of this statement, \
                     so the read is a second value with the same body to run. Bind the owner \
                     first, then read through the binding."
                ),
            );
            return;
        }
        self.drop_extractions.push(DropExtraction {
            expr,
            source: self.source,
            span,
            ty,
        });
    }

    /// Whether `expr` reads storage this frame can name: a local, or a member
    /// or element walk rooted at one.
    ///
    /// This is the same reach the backends have. Native addresses such a walk
    /// and reads through it, which is what lets a member read of a value that
    /// runs a user `Drop` copy nothing. Anything else is a value the expression
    /// computed, which only the expression owns.
    fn reads_a_place(&self, expr: HirExprId) -> bool {
        match *self.program.expr(expr) {
            HirExpr::Local { .. } => true,
            HirExpr::Field { base, .. } | HirExpr::Index { base, .. } => self.reads_a_place(base),
            _ => false,
        }
    }

    /// Takes back a recorded read that a non-consuming position claimed.
    ///
    /// Three positions read a member without owning what they read: the base of
    /// a further member or element read, the receiver of a method that does not
    /// mutate it, and an argument a callee borrows. Each compiles to a borrowed
    /// read that leaves the container holding its value.
    pub(crate) fn excuse_drop_extraction(&mut self, expr: HirExprId) {
        self.drop_extractions.retain(|entry| entry.expr != expr);
    }

    /// The recorded reads, for a probe that analyzes an expression to learn its
    /// type and then throws the answer away.
    ///
    /// A probe both records reads of its own and excuses ones recorded before
    /// it, so the state it has to be rolled back to is the whole list rather
    /// than its length.
    pub(crate) fn drop_extraction_snapshot(&self) -> Vec<DropExtraction> {
        self.drop_extractions.clone()
    }

    /// Puts back the reads a probe found, discarding what it recorded.
    pub(crate) fn restore_drop_extractions(&mut self, snapshot: Vec<DropExtraction>) {
        self.drop_extractions = snapshot;
    }

    /// Refuses every recorded read no position claimed.
    ///
    /// Called once a body is analyzed, which is the first moment every
    /// enclosing expression exists — a read is excused by the expression built
    /// *around* it, so nothing before then can tell a consuming read from a
    /// borrowed one.
    pub(crate) fn report_drop_extractions(&mut self) {
        let here = self.source;
        for entry in std::mem::take(&mut self.drop_extractions) {
            self.source = entry.source;
            self.refuse_drop_extraction(entry.ty, entry.span);
        }
        self.source = here;
    }

    /// Refuses one read that takes a value running a user `Drop` out of the
    /// value holding it.
    ///
    /// Reached from the deferred list, and directly from a desugar that binds a
    /// container's element without building a read for anything to claim — a
    /// `for x in xs` cursor is one.
    pub(crate) fn refuse_drop_extraction(&mut self, ty: Type, span: Span) {
        let name = self.type_name(ty);
        self.emit(
            span,
            "KSEM302",
            format!(
                "this takes a `{name}` out of the value that owns it, and `{name}` runs a user \
                 `Drop` body: the value taken here is a second one with the same body to run, \
                 for storage that only goes away once. Read a member of it, pass it to something \
                 that borrows it, or move the whole owner."
            ),
        );
    }

    /// Refuses a value that runs a user `Drop` at an engine boundary.
    ///
    /// A hybrid program runs two halves with two heaps, and a value crossing
    /// between them is marshalled: the callee gets a copy built from the
    /// bytes, and the caller keeps what it had. A value with a body to run has
    /// no copy — that is the rule every read in this module enforces — so the
    /// crossing is refused rather than run twice or, as the seam would
    /// otherwise do, lost with the body never entered.
    ///
    /// Asked of a function that *declares* an engine, because that declaration
    /// is what makes a crossing possible: an unannotated function compiles for
    /// whichever half its caller is in.
    pub(crate) fn refuse_drop_across_engines(
        &mut self,
        callables: &[crate::analyze::Callable<'_>],
    ) {
        let here = self.source;
        for (index, callable) in callables.iter().enumerate() {
            let Some(engine) = callable.function.execution.annotation() else {
                continue;
            };
            // A specialization copy carries the original's annotation and would
            // report the same declaration again.
            if !callable.specialize.is_empty() {
                continue;
            }
            let id = kira_semantics_model::hir::FuncId(index as u32);
            let sig = &self.sigs[index];
            let (name_span, source) = (sig.name_span, sig.source);
            let crossing: Vec<Type> = self
                .param_types(id)
                .into_iter()
                .chain(std::iter::once(self.signature_return_type(id)))
                .filter(|ty| self.program.types.runs_user_drop(*ty))
                .collect();
            let Some(ty) = crossing.first().copied() else {
                continue;
            };
            self.source = source;
            let name = self.type_name(ty);
            self.emit(
                name_span,
                "KSEM307",
                format!(
                    "`@{engine}` puts this body on one engine, and `{name}` runs a user `Drop` \
                     body, so it cannot cross to it: a value crossing between engines is \
                     marshalled as a copy, and a value with a body to run has no copy. Keep the \
                     value in one half, and cross what it computes."
                ),
            );
            self.source = here;
        }
        self.source = here;
    }
}
