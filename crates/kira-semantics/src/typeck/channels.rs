//! `Channel<T>()`: an ordered handoff between two contexts.
//!
//! A channel is the language's answer to a question a program cannot otherwise
//! ask: how its own work orders against work already running somewhere else.
//! Before it, an outsider could only enqueue and hope; the lifecycle harness
//! found exactly that hole. A receive turns the ordering question into a data
//! dependency, which is the one form of it that has an answer.
//!
//! # What is minted
//!
//! Four kinds of row, all the compiler's own and none spellable in source: a
//! `Sender<T>` and a `Receiver<T>` per payload type, one `ChannelError` for the
//! whole program, and a `Result`-shaped row per payload that a receive answers
//! with. They are minted rather than taken from Foundation for the reason a
//! cast's failure is: a channel is a language construct, so a program that
//! imports nothing still writes one, and a failure it cannot name is a failure
//! it cannot handle.
//!
//! # Why the ends are `distinct` rows
//!
//! An end is one word, and it has to stay one word: an end is moved into the
//! task that uses it, and a task argument slot is a single machine word. A
//! `distinct` over `Int` is exactly that shape with a nominal identity on top,
//! so `Sender<Int>` and `Receiver<Int>` are two types and neither is the `Int`
//! underneath. Passing a receiver where a sender belongs is a type error rather
//! than a direction the runtime has to catch.
//!
//! # Why a closed channel is not a trap
//!
//! The sender being gone is an ordinary end to a conversation, not a program
//! error. A receive on a drained closed channel answers
//! `Error(ChannelError.Closed)`, which the enclosing `handle` covers like every
//! other failure. Sending to a channel whose receiver is gone *is* a trap: the
//! value has nowhere to arrive and nobody to tell.

use kira_semantics_model::channel as wire;
use kira_semantics_model::hir::{HirExpr, HirExprId};
use kira_semantics_model::{DistinctId, EnumDef, EnumId, Instantiation, Type, VariantDef};
use kira_source::Span;
use kira_syntax_model::ast::CallArg;

use crate::analyze::{Analyzer, FnCtx};
use crate::traits::markers::Marker;

/// Which end of a channel a minted row is, and what it carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ChannelEnd {
    /// Whether this row sends or receives.
    pub(crate) direction: Direction,
    /// The payload type the channel carries.
    pub(crate) payload: Type,
}

/// Which way a channel end points.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Direction {
    /// Values go in here.
    Sender,
    /// Values come out here.
    Receiver,
}

impl Analyzer<'_> {
    /// Analyzes `Channel<T>()`, which yields the channel's sender end.
    ///
    /// One expression yields one value, and a channel has two ends, so the
    /// construction names the end a program almost always keeps: the receiver
    /// is read off it as `.receiver`. That is a derivation rather than a second
    /// creation, so there is exactly one channel however many times it is read.
    pub(crate) fn analyze_channel_create(
        &mut self,
        ctx: &mut FnCtx,
        type_args: &[Type],
        args: &[CallArg],
        span: Span,
    ) -> HirExprId {
        for arg in args {
            self.analyze_expr(ctx, arg.value);
        }
        if !args.is_empty() {
            self.emit(
                span,
                "KSEM364",
                "`Channel<T>()` takes no arguments: the payload type is the type argument",
            );
            return self.program.exprs.alloc(HirExpr::Error);
        }
        let [payload] = type_args else {
            self.emit(
                span,
                "KSEM364",
                "`Channel<T>()` takes exactly one type argument, the payload a value crossing it has",
            );
            return self.program.exprs.alloc(HirExpr::Error);
        };
        let payload = *payload;
        if let Some((code, reason)) = self.channel_payload_refusal(payload) {
            self.emit(span, code, reason);
            return self.program.exprs.alloc(HirExpr::Error);
        }
        let Some(sender) = self.channel_end_row(payload, Direction::Sender) else {
            return self.program.exprs.alloc(HirExpr::Error);
        };
        self.program.exprs.alloc(HirExpr::ChannelCreate {
            ty: Type::Distinct(sender),
        })
    }

    /// The scalar a payload's queued word is, with any `distinct` resolved.
    ///
    /// A distinct type is erased before IR exists, so what crosses is the
    /// representation. The declared type is what the success variant carries;
    /// this is what the word holds.
    fn channel_wire_type(&self, payload: Type) -> Type {
        match payload {
            Type::Distinct(id) => self
                .program
                .types
                .distincts()
                .representation(id)
                .unwrap_or(Type::INT),
            other => other,
        }
    }

    /// Why a value of this type cannot cross a channel, with the code the
    /// refusal is filed under, or `None` when it can.
    ///
    /// Two rules in the order the task-slot pair uses them, and for the same
    /// reason: what a value *is* is decided before what it is made of.
    ///
    /// `Send` first. A payload leaves the context that sent it and arrives in
    /// one that context does not own, which is the boundary `Send` describes:
    /// a value naming storage its own engine keeps — a capture cell, a
    /// native-state token, a row in an executor's table — is not one to hand
    /// across, whatever its width. This rule is not narrower than the
    /// representation rule below; it is a different question, and the day a
    /// heap payload is admitted it is the only one still asking it.
    ///
    /// The representation rule second. The queue holds one machine word per
    /// value, so a payload is a scalar with a word of its own: an integer
    /// width, a float width, `Bool`, or a `distinct` over one. `Void` is a
    /// scalar with nothing in it, so a channel over it would carry no value at
    /// all; a pointer word names storage on the far side of a seam this
    /// language does not read, so it is not a value to hand over either.
    fn channel_payload_refusal(&self, payload: Type) -> Option<(&'static str, String)> {
        let name = self.program.types.type_name(payload);
        if let Some(reason) = self.marker_reason(&name, payload, Marker::Send) {
            return Some((
                "KSEM312",
                format!(
                    "a channel cannot carry `{name}`, which cannot cross into the context that \
                     receives it: {reason}"
                ),
            ));
        }
        if payload.is_scalar()
            && !matches!(payload, Type::Void | Type::RawPtr | Type::ForeignPtr(_))
        {
            return None;
        }
        Some((
            "KSEM365",
            format!(
                "a channel carrying `{name}` has nothing to queue: a payload is an integer width, \
                 a float width, `Bool`, or a `distinct` over one, because a queued value is one \
                 machine word"
            ),
        ))
    }

    /// The row for one end of a channel over `payload`, minted on first use.
    fn channel_end_row(&mut self, payload: Type, direction: Direction) -> Option<DistinctId> {
        let known = match direction {
            Direction::Sender => self.channel_senders.get(&payload),
            Direction::Receiver => self.channel_receivers.get(&payload),
        };
        if let Some(&id) = known {
            return Some(id);
        }
        let payload_name = self.program.types.type_name(payload);
        let name = match direction {
            Direction::Sender => wire::sender_name(&payload_name),
            Direction::Receiver => wire::receiver_name(&payload_name),
        };
        // Filed under the compiler's own owner, so a program declaring its own
        // `Sender<Int>` gets its own row rather than colliding with this one.
        // The handle word is the representation: every end is one index into
        // the table the runtime owns, whatever it carries.
        let id = self.program.types.distincts_mut().declare_owned(
            Some(wire::OWNING_MODULE),
            name,
            Type::INT,
        )?;
        self.program
            .types
            .distincts_mut()
            .set_module(id, wire::OWNING_MODULE);
        match direction {
            Direction::Sender => self.channel_senders.insert(payload, id),
            Direction::Receiver => self.channel_receivers.insert(payload, id),
        };
        self.channel_ends
            .insert(id, ChannelEnd { direction, payload });
        Some(id)
    }

    /// The end row `Sender<T>` or `Receiver<T>` names, minting it on first
    /// use, or `None` for any other spelling.
    ///
    /// Minting here rather than only at `Channel<T>()` is what lets a function
    /// declare an end parameter before the file that creates one is analyzed.
    ///
    /// A payload no channel may carry is refused *here*, under the same code
    /// `Channel<T>()` would file it under, rather than left to fall through to
    /// the template lookup: `Sender<String>` is a channel written wrong, not a
    /// name that is not generic. It falls through only when the program
    /// declares a template of that name, which is the case the fall-through
    /// exists for.
    pub(crate) fn channel_end_named(
        &mut self,
        text: &str,
        args: &[Type],
        span: Span,
    ) -> Option<Type> {
        let direction = match text {
            "Sender" => Direction::Sender,
            "Receiver" => Direction::Receiver,
            _ => return None,
        };
        let [payload] = args else {
            return None;
        };
        if let Some((code, reason)) = self.channel_payload_refusal(*payload) {
            if self.program_declares_template(text) {
                return None;
            }
            self.emit(span, code, reason);
            return Some(Type::Error);
        }
        let id = self.channel_end_row(*payload, direction)?;
        Some(Type::Distinct(id))
    }

    /// Whether the program declares a generic template under this name.
    ///
    /// The compiler's rows are owner-filed, so a program is free to declare its
    /// own `Sender`; this is what tells the two apart at a use site.
    fn program_declares_template(&self, text: &str) -> bool {
        self.generic_enum_named(text).is_some()
            || self.generic_aggregate_named(text).is_some()
            || self.traits.contains_key(text)
    }

    /// What a minted end row is, or `None` when this distinct type is a
    /// program's own.
    pub(crate) fn channel_end_of(&self, id: DistinctId) -> Option<ChannelEnd> {
        self.channel_ends.get(&id).copied()
    }

    /// The program's `ChannelError`, minted on first use.
    ///
    /// One row for the whole program: a receive fails one way, and a handler
    /// written once covers every receive in its `attempt`.
    fn channel_error_enum(&mut self) -> Option<EnumId> {
        if let Some(known) = self.channel_error {
            return Some(known);
        }
        let id = self.program.types.enums_mut().declare(EnumDef {
            name: wire::CHANNEL_ERROR.to_owned(),
            variants: vec![VariantDef {
                name: "Closed".to_owned(),
                // Nothing to carry: the receiver already knows which channel it
                // asked, and "closed and drained" has no further detail.
                payload: None,
            }],
        })?;
        self.program
            .types
            .enums_mut()
            .set_module(id, wire::OWNING_MODULE);
        self.channel_error = Some(id);
        Some(id)
    }

    /// The `Result`-shaped row a receive of `payload` answers with.
    fn channel_result_enum(&mut self, payload: Type) -> Option<EnumId> {
        if let Some(&known) = self.channel_results.get(&payload) {
            return Some(known);
        }
        let failure = self.channel_error_enum()?;
        let payload_name = self.program.types.type_name(payload);
        let id = self.program.types.enums_mut().declare(EnumDef {
            name: wire::result_name(&payload_name),
            variants: vec![
                VariantDef {
                    name: "Ok".to_owned(),
                    payload: Some(payload),
                },
                VariantDef {
                    name: "Error".to_owned(),
                    payload: Some(Type::Enum(failure)),
                },
            ],
        })?;
        self.program
            .types
            .enums_mut()
            .set_module(id, wire::OWNING_MODULE);
        self.program.types.enums_mut().record_instantiation(
            id,
            Instantiation {
                template: wire::RESULT_TEMPLATE.to_owned(),
                arguments: vec![payload],
            },
        );
        self.channel_results.insert(payload, id);
        Some(id)
    }

    /// Analyzes a property read on a channel end: `.receiver` and nothing else.
    ///
    /// An end is opaque, so anything else read off one is refused here rather
    /// than falling through to a `distinct`'s `.raw`, which would hand a
    /// program the table index and let it forge an end.
    pub(crate) fn analyze_channel_property(
        &mut self,
        base: HirExprId,
        end: ChannelEnd,
        name: &str,
        span: Span,
    ) -> HirExprId {
        if name != "receiver" || end.direction != Direction::Sender {
            return self.refuse_channel_use(end, name, span);
        }
        let Some(receiver) = self.channel_end_row(end.payload, Direction::Receiver) else {
            return self.program.exprs.alloc(HirExpr::Error);
        };
        self.program.exprs.alloc(HirExpr::ChannelReceiver {
            sender: base,
            ty: Type::Distinct(receiver),
        })
    }

    /// Analyzes a method call on a channel end: `.send(value)`, `.receive()`,
    /// and `.close()`.
    pub(crate) fn analyze_channel_method(
        &mut self,
        ctx: &mut FnCtx,
        receiver_hir: HirExprId,
        end: ChannelEnd,
        name: &str,
        args: &[CallArg],
        span: Span,
    ) -> HirExprId {
        match (name, end.direction) {
            ("send", Direction::Sender) => {
                self.analyze_channel_send(ctx, receiver_hir, end, args, span)
            }
            ("receive", Direction::Receiver) => {
                self.expect_no_arguments(ctx, name, args, span);
                self.analyze_channel_receive(receiver_hir, end, span)
            }
            ("close", _) => {
                self.expect_no_arguments(ctx, name, args, span);
                self.program.exprs.alloc(HirExpr::ChannelClose {
                    end: receiver_hir,
                    sender: end.direction == Direction::Sender,
                })
            }
            _ => {
                for arg in args {
                    self.analyze_expr(ctx, arg.value);
                }
                self.refuse_channel_use(end, name, span)
            }
        }
    }

    /// `sender.send(value)`: one value onto the back of the queue.
    fn analyze_channel_send(
        &mut self,
        ctx: &mut FnCtx,
        sender: HirExprId,
        end: ChannelEnd,
        args: &[CallArg],
        span: Span,
    ) -> HirExprId {
        let [arg] = args else {
            for arg in args {
                self.analyze_expr(ctx, arg.value);
            }
            self.emit(
                span,
                "KSEM366",
                "`send` takes exactly one value, the one crossing the channel",
            );
            return self.program.exprs.alloc(HirExpr::Error);
        };
        let value = self.analyze_expr_expecting(ctx, arg.value, Some(end.payload));
        let actual = self.program.expr(value).type_of();
        if !actual.assignable_to(end.payload) {
            let expected = self.program.types.type_name(end.payload);
            let found = self.program.types.type_name(actual);
            self.emit(
                self.tree.expr(arg.value).span(),
                "KSEM063",
                format!("expected `{expected}`, found `{found}`"),
            );
        }
        let wire = self.channel_wire_type(end.payload);
        self.program.exprs.alloc(HirExpr::ChannelSend {
            sender,
            value,
            wire,
        })
    }

    /// `receiver.receive()`: the next value, or the channel's end.
    ///
    /// Answers a `Result`-shaped value the `attempt` machinery consumes, so it
    /// needs no rule of its own for exhaustiveness or handler resolution: it
    /// produces what a fallible call produces.
    fn analyze_channel_receive(
        &mut self,
        receiver: HirExprId,
        end: ChannelEnd,
        span: Span,
    ) -> HirExprId {
        let Some(result) = self.channel_result_enum(end.payload) else {
            return self.program.exprs.alloc(HirExpr::Error);
        };
        let Some(failure) = self.channel_error_enum() else {
            return self.program.exprs.alloc(HirExpr::Error);
        };
        let _ = span;
        let wire = self.channel_wire_type(end.payload);
        self.program.exprs.alloc(HirExpr::ChannelReceive {
            receiver,
            payload: end.payload,
            wire,
            failure,
            ty: Type::Enum(result),
        })
    }

    /// Analyzes and discards arguments a no-argument operation was handed.
    fn expect_no_arguments(&mut self, ctx: &mut FnCtx, name: &str, args: &[CallArg], span: Span) {
        for arg in args {
            self.analyze_expr(ctx, arg.value);
        }
        if !args.is_empty() {
            self.emit(span, "KSEM366", format!("`{name}` takes no arguments"));
        }
    }

    /// Reports an operation a channel end does not have.
    fn refuse_channel_use(&mut self, end: ChannelEnd, name: &str, span: Span) -> HirExprId {
        let (kind, has) = match end.direction {
            Direction::Sender => ("sender", "`send(value)`, `close()`, and `.receiver`"),
            Direction::Receiver => ("receiver", "`receive()` and `close()`"),
        };
        self.emit(
            span,
            "KSEM367",
            format!("a channel {kind} has no `{name}`; it has {has}"),
        );
        self.program.exprs.alloc(HirExpr::Error)
    }
}
