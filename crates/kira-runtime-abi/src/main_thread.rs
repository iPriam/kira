//! The host contract for work that must run on the process main thread.
//!
//! The language does not know about a particular UI toolkit or async runtime.
//! It only knows this platform capability: a Kira function may be requested on
//! the host's main thread, and the host owns the event loop that services the
//! request. The request carries an owned value tree so the helper thread never
//! lends heap storage to the main thread.

use thiserror::Error;

use crate::NativeStateValue;

/// One operation on the host's main-thread event loop.
///
/// These bytes are part of the runtime contract used by bytecode and native
/// lowering. Keep the numbering append-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum MainThreadOp {
    /// Run a function synchronously and return its value.
    Invoke = 0,
    /// Queue a function and return a handle that can be joined later.
    Spawn = 1,
    /// Queue a function without retaining a result.
    Post = 2,
    /// Start a long-lived lifecycle on its own preserved main-thread stack.
    LifecycleStart = 3,
}

impl MainThreadOp {
    /// Every operation in wire order.
    pub const ALL: [Self; 4] = [Self::Invoke, Self::Spawn, Self::Post, Self::LifecycleStart];

    /// The byte used when this operation is serialized.
    pub const fn as_byte(self) -> u8 {
        self as u8
    }

    /// Decodes one operation byte.
    pub fn from_byte(byte: u8) -> Option<Self> {
        Self::ALL.get(usize::from(byte)).copied()
    }

    /// The source-level operation name.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Invoke => "invoke",
            Self::Spawn => "spawn",
            Self::Post => "post",
            Self::LifecycleStart => "lifecycle start",
        }
    }
}

/// An opaque handle to one queued main-thread task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MainThreadHandle(u64);

impl MainThreadHandle {
    /// The reserved invalid handle.
    pub const NULL: Self = Self(0);

    /// Builds a handle from its runtime word.
    pub const fn from_word(word: u64) -> Self {
        Self(word)
    }

    /// Returns the runtime word used by host implementations.
    pub const fn word(self) -> u64 {
        self.0
    }
}

/// An owned request sent to the main-thread event loop.
#[derive(Debug, Clone, PartialEq)]
pub struct MainThreadRequest {
    /// The requested scheduling operation.
    pub operation: MainThreadOp,
    /// The function id in the current Kira program.
    pub function: u32,
    /// Values copied out of the requesting VM/native context.
    pub args: Vec<NativeStateValue>,
}

impl MainThreadRequest {
    /// Builds a request whose arguments are owned by the request.
    pub fn new(operation: MainThreadOp, function: u32, args: Vec<NativeStateValue>) -> Self {
        Self {
            operation,
            function,
            args,
        }
    }
}

/// The answer to one main-thread request.
#[derive(Debug, Clone, PartialEq)]
pub enum MainThreadResponse {
    /// The synchronous invocation's owned result.
    Value(NativeStateValue),
    /// The handle allocated for a spawned task.
    Spawned(MainThreadHandle),
    /// The post was accepted by the event loop.
    Posted,
}

/// Why a main-thread request could not be completed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum MainThreadError {
    /// The host does not provide a main-thread event loop.
    #[error("this host does not provide a main-thread event loop")]
    NoHost,
    /// The request named no function in the loaded program.
    #[error("main-thread request names unknown function {0}")]
    UnknownFunction(u32),
    /// A handle did not name a queued task.
    #[error("main-thread task handle {0} is unknown or already joined")]
    UnknownHandle(u64),
    /// The target was not in the state expected by the operation.
    #[error("main-thread operation `{operation}` received an incompatible response")]
    WrongResponse {
        /// The operation whose response was checked.
        operation: &'static str,
    },
    /// The target function failed while it was being serviced by the loop.
    #[error("main-thread function failed: {0}")]
    Function(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_wire_bytes_are_append_only() {
        assert_eq!(MainThreadOp::Invoke.as_byte(), 0);
        assert_eq!(MainThreadOp::Spawn.as_byte(), 1);
        assert_eq!(MainThreadOp::Post.as_byte(), 2);
        assert_eq!(MainThreadOp::LifecycleStart.as_byte(), 3);
        for operation in MainThreadOp::ALL {
            assert_eq!(
                MainThreadOp::from_byte(operation.as_byte()),
                Some(operation)
            );
        }
        assert_eq!(MainThreadOp::from_byte(4), None);
    }

    #[test]
    fn handles_preserve_their_runtime_word() {
        let handle = MainThreadHandle::from_word(41);
        assert_eq!(handle.word(), 41);
        assert_ne!(handle, MainThreadHandle::NULL);
    }
}
