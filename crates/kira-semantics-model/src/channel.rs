//! The shapes the channel surface is built from.
//!
//! Analysis mints the rows and both backends read the representation, so the
//! names and the wire meaning of a handle word are stated once here rather
//! than rediscovered in three places.
//!
//! # Why the ends are distinct types
//!
//! A channel end is one machine word: an index into the table the runtime
//! owns. Minting each end as a `distinct` over `Int` gives it a nominal
//! identity the type checker keeps apart from every other number, and gives it
//! scalar layout for free. Scalar layout is not a convenience here, it is the
//! requirement: an end is moved into the task that uses it, and a task
//! argument slot is one word.
//!
//! A `Sender<Int>` and a `Receiver<Int>` are therefore two rows, and neither
//! is the `Int` underneath: passing a receiver where a sender belongs is a
//! type error rather than a direction the runtime has to catch.

/// The module the minted rows are attributed to.
///
/// Not Foundation's and not spellable in source: a channel is a language
/// construct, so a program that imports nothing still writes one.
pub const OWNING_MODULE: &str = "Kira";

/// The template identity every sender row records.
pub const SENDER_TEMPLATE: &str = "Kira::Sender";

/// The template identity every receiver row records.
pub const RECEIVER_TEMPLATE: &str = "Kira::Receiver";

/// The name a sender row over `payload` is declared under.
pub fn sender_name(payload: &str) -> String {
    format!("Sender<{payload}>")
}

/// The name a receiver row over `payload` is declared under.
pub fn receiver_name(payload: &str) -> String {
    format!("Receiver<{payload}>")
}

/// How far a receiver's handle sits from its sender's.
///
/// The two ends of a channel share an index and a generation and differ only in
/// the end bit of the slot field, which is what lets `Create` yield one word
/// that names both. The slot field is 1-based so that zero can mean "no
/// channel", and that offset turns "set the end bit" into "add one": a sender's
/// field is `(index << 1) + 1` and its receiver's is `(index << 1) + 2`.
/// Generated code derives one from the other by adding this, so the value is a
/// wire contract between the minting analysis and the runtime table.
pub const RECEIVER_END_OFFSET: i64 = 1;

/// What a poll answers when the queue is empty and the sender is live.
pub const POLL_EMPTY: i64 = 0;

/// What a poll answers when a value is waiting.
pub const POLL_READY: i64 = 1;

/// What a poll answers when the queue is drained and the sender is gone.
pub const POLL_CLOSED: i64 = 2;

/// The name the compiler's channel-failure enum is declared under.
///
/// A receive on a closed channel is not a trap: the sender being gone is an
/// ordinary end to a conversation, and the receiver is the one place a program
/// can act on it. So it is a typed failure a `handle` covers, exactly as a
/// tried cast's mismatch is.
pub const CHANNEL_ERROR: &str = "ChannelError";

/// `ChannelError.Closed`, carrying nothing.
///
/// There is one way a receive fails and no detail to carry: the channel is
/// closed and drained, which the receiver already knows the identity of.
pub const CLOSED_TAG: u32 = 0;

/// The template identity the per-payload receive-result rows record.
pub const RESULT_TEMPLATE: &str = "Kira::ReceiveResult";

/// `Ok(payload)`.
pub const OK_TAG: u32 = 0;

/// `Error(ChannelError)`.
pub const ERROR_TAG: u32 = 1;

/// The name the receive-result row over `payload` is declared under.
pub fn result_name(payload: &str) -> String {
    format!("ReceiveResult<{payload}>")
}

/// How a payload's value becomes the one machine word a queue slot holds.
///
/// A queue slot is a word, and this says which of three things that word is,
/// once, rather than leaving each of the two ends to work it out from the
/// payload type and risk working it out differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Crossing {
    /// The value *is* the word: an integer width, `Bool`, or a `distinct`
    /// over one.
    Word,
    /// The word is the value's IEEE-754 bits, converted back on arrival, so a
    /// `Float` reads out as itself.
    FloatBits,
    /// The word is a native-state token naming the value, which lives in the
    /// store while it is queued.
    ///
    /// A payload that owns storage has no word of its own, so it travels as a
    /// handle to storage that outlives the sending context. The store is the
    /// one the callback seam already uses, and it is the right one for exactly
    /// the reason it was built: it holds a Kira value across a boundary that
    /// cannot carry one.
    ///
    /// The id is not minted. `TypeTable::native_state_type_id` derives it from
    /// the type — fingerprinting a struct, array or enum's shape rather than
    /// its table index — so the sender's box and the receiver's recovery agree
    /// by construction rather than by protocol, and neither can drift.
    Boxed(kira_runtime_abi::NativeStateTypeId),
}
