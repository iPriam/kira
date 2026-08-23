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

use kira_semantics_model::{StructId, Type};
use kira_source::{SourceId, Span};

use crate::analyze::Analyzer;

/// The one member the `Drop` trait declares.
pub(crate) const DROP_MEMBER: &str = "drop";

impl Analyzer<'_> {
    /// Validates every `Drop` conformance and records the body each one names.
    ///
    /// Runs after signatures, because the body is a method and a method has no
    /// id until then, and before any body is analyzed, because whether a type
    /// runs a user drop decides whether it is released at all.
    pub(crate) fn record_user_drops(&mut self) {
        let claims: Vec<(StructId, SourceId, Span)> = self
            .conformances
            .iter()
            .filter(|entry| entry.trait_name == super::DROP)
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
}
