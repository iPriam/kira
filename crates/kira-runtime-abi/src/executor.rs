//! Cooperative single-threaded async executor (shared runtime core).
//!
//! Ported from kira-zig `kira_runtime_abi/src/executor.zig`: a FIFO
//! ready-queue of poll-driven tasks and a `block_on` drive loop. A task that
//! returns `Pending` yields control and is re-enqueued at the tail so
//! independent tasks interleave.
//!
//! The Zig executor threads an intrusive `next: ?*Task` list through the task
//! control blocks; the Rust port owns the tasks in a vector and queues
//! [`TaskId`] indices instead (index/arena port rule).

use std::collections::VecDeque;

use crate::task::{Task, TaskId};

/// Cooperative FIFO executor (Zig `Executor`).
#[derive(Debug, Default)]
pub struct Executor {
    /// Task storage; a [`TaskId`] indexes this vector (replaces the Zig
    /// allocator-owned `owned` list + intrusive queue links).
    pub tasks: Vec<Task>,
    /// FIFO ready-queue of task indices (Zig intrusive `head`/`tail` list).
    pub ready: VecDeque<TaskId>,
}

// TODO(port): `spawn`, `enqueue`, `tick`, `run_to_idle`, and `block_on`
// (kira-zig executor.zig) — the cooperative drive loop, re-enqueue-on-pending
// semantics, and cooperative-cancel observation.
