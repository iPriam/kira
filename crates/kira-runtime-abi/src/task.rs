//! Shared async task ABI — the `Task` layout and poll protocol that both the
//! VM and the LLVM/native backend agree on (one task model, two backends).
//!
//! Ported from kira-zig `kira_runtime_abi/src/task.zig`. A `Task` is a
//! resumable, poll-driven state machine: the executor advances it via
//! `poll_fn`; the task either completes (`Ready`) or suspends (`Pending`) at a
//! cooperative yield/park point.

use crate::value::Value;

/// Index of a task inside its [`crate::executor::Executor`].
///
/// The Zig `Task` carries an intrusive `next: ?*Task` FIFO link; the Rust port
/// follows the index/arena rule instead — the executor owns the tasks and its
/// ready-queue holds `TaskId`s.
pub type TaskId = u32;

/// Result of advancing a task one step (Zig `Poll`).
#[derive(Debug, Clone, PartialEq)]
pub enum Poll {
    /// Zig `.ready: Value` — the task finished and produced its value.
    Ready(Value),
    /// Zig `.pending` — the task suspended and must be polled again later.
    Pending,
}

/// Advances a task one step (Zig `PollFn`). Implementations read/write
/// `task.context` (the state-machine frame) and must observe
/// `task.cancel_requested` at their cooperative cancel points.
pub type PollFn = fn(task: &mut Task) -> Poll;

/// Lifecycle state of a task (Zig `TaskState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TaskState {
    /// Zig `.ready` — enqueued (or freshly spawned) and waiting to be polled.
    #[default]
    Ready,
    /// Zig `.running` — currently being polled by the executor.
    Running,
    /// Zig `.suspended` — suspended at a yield/park point; waiting to be re-enqueued.
    Suspended,
    /// Zig `.complete` — finished with a result (possibly cancellation-observed).
    Complete,
}

/// The task control block (Zig `Task`). Layout is intentionally flat and
/// backend-neutral so generated code on either backend can allocate and
/// initialize it identically.
#[derive(Debug)]
pub struct Task {
    /// Zig `poll_fn: PollFn`.
    pub poll_fn: PollFn,
    /// Zig `context: ?*anyopaque` — opaque state-machine frame owned by the
    /// task's generated code.
    pub context: *mut core::ffi::c_void,
    /// Zig `state: TaskState = .ready`.
    pub state: TaskState,
    /// Zig `cancel_requested: bool` — cooperative-cancel flag; setting it
    /// never force-terminates a running task.
    pub cancel_requested: bool,
    /// Zig `cancelled: bool` — whether cancellation was observed before completion.
    pub cancelled: bool,
    /// Zig `result: Value = .void` — completed value; valid once `state == Complete`.
    pub result: Value,
}

impl Task {
    /// Creates a fresh task (Zig `Task.init`).
    pub fn new(poll_fn: PollFn, context: *mut core::ffi::c_void) -> Task {
        Task {
            poll_fn,
            context,
            state: TaskState::Ready,
            cancel_requested: false,
            cancelled: false,
            result: Value::Void,
        }
    }

    /// Cooperative cancellation (Zig `requestCancel`): request the task stop
    /// at its next cancel point. A no-op on an already-complete task.
    pub fn request_cancel(&mut self) {
        if self.state == TaskState::Complete {
            return;
        }
        self.cancel_requested = true;
    }

    /// True once the task has completed (Zig `isComplete`).
    pub fn is_complete(&self) -> bool {
        self.state == TaskState::Complete
    }
}
