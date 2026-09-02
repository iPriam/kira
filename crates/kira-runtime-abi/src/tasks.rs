//! The deferred-task executor both engines run, and the primitive tags that
//! reach it.
//!
//! Kira's async spine is a *compiler* construct. `Task { f(a, b) }`, `.await`,
//! `.requestCancel()`, `.detach()`, `taskYield()`, and `taskSleep(ms)` all
//! lower to ordinary Kira functions the IR synthesizes, and those synthesized
//! functions reach the executor through exactly one primitive —
//! [`TaskPrim`] — carried by one bytecode instruction on the VM and one
//! `kira_rt_task_op` call in native code. The scheduling *policy* therefore
//! lives in generated Kira, not here: this type only owns the task table.
//!
//! That is why the table lives in layer 0 rather than twice. Which joins trap,
//! what a handle means, and when a cancelled task stops being runnable are
//! *semantics*, and a second copy of them in the native runtime would be a
//! parity bug waiting to be written. `kira-vm-runtime` owns one
//! [`TaskExecutor`] per run and `kira-native-bridge` owns one per thread; both
//! see the same calls in the same order, because the generated code that makes
//! them is the same IR.
//!
//! # The clock is virtual
//!
//! `taskSleep(ms)` moves a monotonic counter this executor owns, and nothing
//! sleeps in real time: the VM is a portable core with no thread call available
//! to it, and a native half that really slept would order two programs
//! differently from the VM half of the same program.

/// One primitive of the task executor's runtime interface.
///
/// The discriminants are a wire contract: they travel in the operand byte of
/// the `TaskOp` bytecode instruction and in the first argument of
/// `kira_rt_task_op`, so they are **append-only** — a new primitive takes the
/// next free number and no existing one ever moves.
///
/// Every primitive takes three `Int` operands and yields one. Operands a
/// primitive does not use are passed as zero and ignored, which is what lets
/// one instruction and one native symbol carry the whole surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum TaskPrim {
    /// `(target, _, _)` — create a pending task on dispatcher arm `target`,
    /// yielding its handle.
    Spawn = 0,
    /// `(handle, slot, value)` — write one argument slot of a pending task.
    SetArg = 1,
    /// `(handle, slot, _)` — read one argument slot of a task.
    SlotGet = 2,
    /// `(handle, _, _)` — the dispatcher arm a task's body lives in.
    TargetOf = 3,
    /// `(handle, _, _)` — claim a task for joining: `1` when its body still has
    /// to run, `0` when its result is already waiting. Traps on a join that can
    /// never succeed.
    BeginJoin = 4,
    /// `(handle, _, _)` — claim a task for detaching, with the same answers as
    /// [`TaskPrim::BeginJoin`]. Traps on a task already joined or detached.
    BeginDetach = 5,
    /// `(_, _, _)` — claim the next runnable task, or `0` when none is.
    PickReady = 6,
    /// `(handle, value, _)` — a claimed task's body finished with `value`.
    Complete = 7,
    /// `(handle, _, _)` — take a finished task's result and mark it joined.
    TakeResult = 8,
    /// `(handle, _, _)` — mark a driven task detached; its result is discarded.
    MarkDetached = 9,
    /// `(handle, _, _)` — request cooperative cancellation.
    Cancel = 10,
    /// `(ms, _, _)` — move the virtual clock forward.
    AdvanceClock = 11,
}

impl TaskPrim {
    /// Every primitive, in wire order.
    ///
    /// The one place the set is written down: decoding indexes this rather than
    /// repeating a match, so a new primitive cannot be added to the enum and
    /// forgotten by the decoder.
    pub const ALL: [TaskPrim; 12] = [
        TaskPrim::Spawn,
        TaskPrim::SetArg,
        TaskPrim::SlotGet,
        TaskPrim::TargetOf,
        TaskPrim::BeginJoin,
        TaskPrim::BeginDetach,
        TaskPrim::PickReady,
        TaskPrim::Complete,
        TaskPrim::TakeResult,
        TaskPrim::MarkDetached,
        TaskPrim::Cancel,
        TaskPrim::AdvanceClock,
    ];

    /// The wire byte this primitive travels as.
    pub const fn as_byte(self) -> u8 {
        self as u8
    }

    /// Reads a wire byte, or `None` when it names no primitive.
    ///
    /// A decoder never guesses: an unknown byte is rejected by its caller
    /// rather than folded into a neighbouring primitive.
    pub fn from_byte(byte: u8) -> Option<Self> {
        Self::ALL.get(usize::from(byte)).copied()
    }

    /// A short name for this primitive, for disassembly and diagnostics.
    pub const fn label(self) -> &'static str {
        match self {
            TaskPrim::Spawn => "spawn",
            TaskPrim::SetArg => "setArg",
            TaskPrim::SlotGet => "slotGet",
            TaskPrim::TargetOf => "targetOf",
            TaskPrim::BeginJoin => "beginJoin",
            TaskPrim::BeginDetach => "beginDetach",
            TaskPrim::PickReady => "pickReady",
            TaskPrim::Complete => "complete",
            TaskPrim::TakeResult => "takeResult",
            TaskPrim::MarkDetached => "markDetached",
            TaskPrim::Cancel => "cancel",
            TaskPrim::AdvanceClock => "advanceClock",
        }
    }
}

/// Why a task primitive could not be carried out.
///
/// Every variant is a program error the language defines as a runtime trap, so
/// each engine renders it its own way — a `VmError` on the VM, a message and a
/// failing exit on native — while agreeing on *which* programs trap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TaskTrap {
    /// A handle that names no task reached a primitive.
    #[error("task handle is not live")]
    UnknownHandle,
    /// A task was joined or detached after it had already been joined.
    #[error("task was already joined")]
    AlreadyJoined,
    /// A task was joined after it had been detached.
    #[error("task was detached, so its result cannot be joined")]
    Detached,
    /// A cancelled task was joined; it has no result and never will.
    #[error("task was cancelled, so it has no result to join")]
    Cancelled,
    /// A task was joined from inside its own body.
    #[error("task is already running, so joining it would never finish")]
    Reentrant,
    /// A result was taken from a task that has not finished.
    #[error("task has not finished, so it has no result to take")]
    NotFinished,
    /// An argument slot outside the fixed per-task set was addressed.
    #[error("task argument slot is out of range")]
    SlotOutOfRange,
    /// The executor cannot represent another stable task identity.
    #[error("task handle space is exhausted")]
    HandleExhausted,
}

/// How many argument slots one task carries.
///
/// A task body is a direct call to a named function with scalar parameters, and
/// this bounds that parameter list. It is a fixed array rather than a `Vec` so
/// spawning allocates once, in the table, and never per task.
pub const TASK_SLOTS: usize = 8;

/// Where a task is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskState {
    /// Spawned; its body has not run.
    Pending,
    /// Claimed by a driver; its body is on the stack right now.
    Running,
    /// Its body finished and its result is waiting to be taken.
    Finished,
    /// Its result was taken.
    Joined,
    /// It was driven and its result thrown away.
    Detached,
    /// Cancellation was requested before its body ever ran.
    Cancelled,
}

/// One spawned task.
#[derive(Debug, Clone, Copy)]
struct Task {
    state: TaskState,
    /// The dispatcher arm this task's body lives in.
    target: i64,
    /// The evaluated arguments, captured at the spawn site.
    slots: [i64; TASK_SLOTS],
    /// The value its body produced, once it has one.
    result: i64,
}

/// One reusable task-table position.
#[derive(Debug, Clone, Copy, Default)]
struct TaskSlot {
    /// Incremented whenever the task in this position is reclaimed.
    generation: u32,
    task: Option<Task>,
}

/// The task table one running program owns.
///
/// A handle packs a generation in its high 32 bits and a 1-based slot index in
/// its low 32 bits. Reusing storage therefore never makes an old handle name a
/// new task, while `0` remains free to mean "no task".
#[derive(Debug, Default)]
pub struct TaskExecutor {
    tasks: Vec<TaskSlot>,
    free: Vec<usize>,
    clock_ms: i64,
}

impl TaskExecutor {
    /// A table with no tasks in it.
    pub fn new() -> Self {
        Self::default()
    }

    /// The virtual clock, in milliseconds since the run began.
    ///
    /// Only [`TaskPrim::AdvanceClock`] moves it, so two runs of one program
    /// read the same value at the same point on every backend.
    pub fn clock_ms(&self) -> i64 {
        self.clock_ms
    }

    /// How many tasks have been spawned, joined or not.
    pub fn spawned(&self) -> usize {
        self.tasks.iter().filter(|slot| slot.task.is_some()).count()
    }

    /// Carries out one primitive.
    ///
    /// The single entry point both engines call, so neither can drift into its
    /// own reading of what a primitive means.
    pub fn perform(&mut self, prim: TaskPrim, a: i64, b: i64, c: i64) -> Result<i64, TaskTrap> {
        match prim {
            TaskPrim::Spawn => self.spawn(a),
            TaskPrim::SetArg => {
                let slot = Self::slot_index(b)?;
                self.task_mut(a)?.slots[slot] = c;
                Ok(0)
            }
            TaskPrim::SlotGet => {
                let slot = Self::slot_index(b)?;
                Ok(self.task(a)?.slots[slot])
            }
            TaskPrim::TargetOf => Ok(self.task(a)?.target),
            TaskPrim::BeginJoin => self.begin_join(a),
            TaskPrim::BeginDetach => self.begin_detach(a),
            TaskPrim::PickReady => Ok(self.pick_ready()),
            TaskPrim::Complete => {
                let task = self.task_mut(a)?;
                task.result = b;
                task.state = TaskState::Finished;
                Ok(0)
            }
            TaskPrim::TakeResult => self.take_result(a),
            TaskPrim::MarkDetached => {
                self.task_mut(a)?.state = TaskState::Detached;
                self.reclaim(a)?;
                Ok(0)
            }
            TaskPrim::Cancel => {
                if self.task(a)?.state == TaskState::Pending {
                    self.task_mut(a)?.state = TaskState::Cancelled;
                    self.reclaim(a)?;
                }
                Ok(0)
            }
            TaskPrim::AdvanceClock => {
                self.clock_ms = self.clock_ms.saturating_add(a.max(0));
                Ok(0)
            }
        }
    }

    /// Adds a pending task and returns its handle.
    fn spawn(&mut self, target: i64) -> Result<i64, TaskTrap> {
        let task = Task {
            state: TaskState::Pending,
            target,
            slots: [0; TASK_SLOTS],
            result: 0,
        };
        let index = match self.free.pop() {
            Some(index) => index,
            None => {
                let index = self.tasks.len();
                u32::try_from(index + 1).map_err(|_| TaskTrap::HandleExhausted)?;
                self.tasks.push(TaskSlot::default());
                index
            }
        };
        let slot = self.tasks.get_mut(index).ok_or(TaskTrap::HandleExhausted)?;
        slot.task = Some(task);
        Self::handle(index, slot.generation)
    }

    /// Claims `handle` for a join, answering whether its body still has to run.
    fn begin_join(&mut self, handle: i64) -> Result<i64, TaskTrap> {
        let task = self.task_mut(handle)?;
        match task.state {
            TaskState::Pending => {
                task.state = TaskState::Running;
                Ok(1)
            }
            TaskState::Finished => Ok(0),
            TaskState::Running => Err(TaskTrap::Reentrant),
            TaskState::Joined => Err(TaskTrap::AlreadyJoined),
            TaskState::Detached => Err(TaskTrap::Detached),
            TaskState::Cancelled => Err(TaskTrap::Cancelled),
        }
    }

    /// Claims `handle` for a detach, answering whether its body still has to
    /// run.
    ///
    /// Detaching a cancelled task is not an error: cancellation already said
    /// the result is unwanted, and detaching says it again.
    fn begin_detach(&mut self, handle: i64) -> Result<i64, TaskTrap> {
        let task = self.task_mut(handle)?;
        match task.state {
            TaskState::Pending => {
                task.state = TaskState::Running;
                Ok(1)
            }
            TaskState::Finished | TaskState::Cancelled => Ok(0),
            TaskState::Running => Err(TaskTrap::Reentrant),
            TaskState::Joined => Err(TaskTrap::AlreadyJoined),
            TaskState::Detached => Err(TaskTrap::Detached),
        }
    }

    /// Claims the oldest pending task, or answers `0` when none is runnable.
    ///
    /// Oldest-first is the round-robin order: a task spawned earlier runs
    /// earlier, and a task already on the stack is `Running`, so a driver can
    /// never pick the body it is standing in.
    fn pick_ready(&mut self) -> i64 {
        for (index, slot) in self.tasks.iter_mut().enumerate() {
            let Some(task) = slot.task.as_mut() else {
                continue;
            };
            if task.state == TaskState::Pending {
                task.state = TaskState::Running;
                return Self::handle(index, slot.generation).unwrap_or(0);
            }
        }
        0
    }

    /// Takes a finished task's result, marking it joined.
    fn take_result(&mut self, handle: i64) -> Result<i64, TaskTrap> {
        match self.task(handle)?.state {
            TaskState::Finished => {
                let result = self.task(handle)?.result;
                self.task_mut(handle)?.state = TaskState::Joined;
                self.reclaim(handle)?;
                Ok(result)
            }
            TaskState::Joined => Err(TaskTrap::AlreadyJoined),
            TaskState::Detached => Err(TaskTrap::Detached),
            TaskState::Cancelled => Err(TaskTrap::Cancelled),
            TaskState::Pending | TaskState::Running => Err(TaskTrap::NotFinished),
        }
    }

    /// Reads a slot index, refusing one outside the fixed set.
    fn slot_index(slot: i64) -> Result<usize, TaskTrap> {
        usize::try_from(slot)
            .ok()
            .filter(|index| *index < TASK_SLOTS)
            .ok_or(TaskTrap::SlotOutOfRange)
    }

    /// Resolves a handle to a live task.
    fn task(&self, handle: i64) -> Result<&Task, TaskTrap> {
        let (index, generation) = Self::parts(handle)?;
        let slot = self.tasks.get(index).ok_or(TaskTrap::UnknownHandle)?;
        if slot.generation != generation {
            return Err(TaskTrap::UnknownHandle);
        }
        slot.task.as_ref().ok_or(TaskTrap::UnknownHandle)
    }

    /// Resolves a handle to a live task for writing.
    fn task_mut(&mut self, handle: i64) -> Result<&mut Task, TaskTrap> {
        let (index, generation) = Self::parts(handle)?;
        let slot = self.tasks.get_mut(index).ok_or(TaskTrap::UnknownHandle)?;
        if slot.generation != generation {
            return Err(TaskTrap::UnknownHandle);
        }
        slot.task.as_mut().ok_or(TaskTrap::UnknownHandle)
    }

    /// Reclaims one terminal task and advances the slot generation.
    fn reclaim(&mut self, handle: i64) -> Result<(), TaskTrap> {
        let (index, generation) = Self::parts(handle)?;
        let slot = self.tasks.get_mut(index).ok_or(TaskTrap::UnknownHandle)?;
        if slot.generation != generation || slot.task.take().is_none() {
            return Err(TaskTrap::UnknownHandle);
        }
        if let Some(next) = slot
            .generation
            .checked_add(1)
            .filter(|next| *next <= i32::MAX as u32)
        {
            slot.generation = next;
            self.free.push(index);
        }
        Ok(())
    }

    /// Packs one stable handle.
    fn handle(index: usize, generation: u32) -> Result<i64, TaskTrap> {
        let slot = u32::try_from(index + 1).map_err(|_| TaskTrap::HandleExhausted)?;
        let word = (u64::from(generation) << 32) | u64::from(slot);
        i64::try_from(word).map_err(|_| TaskTrap::HandleExhausted)
    }

    /// Unpacks and validates a nonzero handle.
    fn parts(handle: i64) -> Result<(usize, u32), TaskTrap> {
        let word = u64::try_from(handle).map_err(|_| TaskTrap::UnknownHandle)?;
        let slot =
            u32::try_from(word & u64::from(u32::MAX)).map_err(|_| TaskTrap::UnknownHandle)?;
        let index = usize::try_from(slot.checked_sub(1).ok_or(TaskTrap::UnknownHandle)?)
            .map_err(|_| TaskTrap::UnknownHandle)?;
        Ok((index, (word >> 32) as u32))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_primitive_wire_bytes_are_pinned() {
        // Spelled out literally: a reorder here silently redirects every
        // already-compiled module, so it has to fail a test instead.
        assert_eq!(TaskPrim::Spawn.as_byte(), 0);
        assert_eq!(TaskPrim::SetArg.as_byte(), 1);
        assert_eq!(TaskPrim::SlotGet.as_byte(), 2);
        assert_eq!(TaskPrim::TargetOf.as_byte(), 3);
        assert_eq!(TaskPrim::BeginJoin.as_byte(), 4);
        assert_eq!(TaskPrim::BeginDetach.as_byte(), 5);
        assert_eq!(TaskPrim::PickReady.as_byte(), 6);
        assert_eq!(TaskPrim::Complete.as_byte(), 7);
        assert_eq!(TaskPrim::TakeResult.as_byte(), 8);
        assert_eq!(TaskPrim::MarkDetached.as_byte(), 9);
        assert_eq!(TaskPrim::Cancel.as_byte(), 10);
        assert_eq!(TaskPrim::AdvanceClock.as_byte(), 11);
    }

    #[test]
    fn every_primitive_round_trips_through_its_byte() {
        for prim in TaskPrim::ALL {
            assert_eq!(TaskPrim::from_byte(prim.as_byte()), Some(prim));
        }
    }

    #[test]
    fn an_unknown_byte_names_no_primitive() {
        assert_eq!(TaskPrim::from_byte(TaskPrim::ALL.len() as u8), None);
        assert_eq!(TaskPrim::from_byte(u8::MAX), None);
    }

    /// Spawns a one-argument task and returns its handle.
    fn spawn_one(executor: &mut TaskExecutor, target: i64, arg: i64) -> i64 {
        let handle = executor.perform(TaskPrim::Spawn, target, 0, 0).unwrap();
        executor
            .perform(TaskPrim::SetArg, handle, 0, arg)
            .expect("slot 0 is in range");
        handle
    }

    #[test]
    fn a_spawned_task_carries_its_target_and_arguments() {
        let mut executor = TaskExecutor::new();
        let handle = spawn_one(&mut executor, 3, 41);
        assert_eq!(handle, 1);
        assert_eq!(executor.perform(TaskPrim::TargetOf, handle, 0, 0), Ok(3));
        assert_eq!(executor.perform(TaskPrim::SlotGet, handle, 0, 0), Ok(41));
        assert_eq!(executor.spawned(), 1);
    }

    #[test]
    fn a_join_drives_once_and_takes_the_result() {
        let mut executor = TaskExecutor::new();
        let handle = spawn_one(&mut executor, 1, 7);
        assert_eq!(executor.perform(TaskPrim::BeginJoin, handle, 0, 0), Ok(1));
        assert_eq!(executor.perform(TaskPrim::Complete, handle, 42, 0), Ok(0));
        assert_eq!(executor.perform(TaskPrim::TakeResult, handle, 0, 0), Ok(42));
    }

    #[test]
    fn a_join_reclaims_the_task_and_stales_its_handle() {
        let mut executor = TaskExecutor::new();
        let handle = spawn_one(&mut executor, 1, 7);
        executor.perform(TaskPrim::BeginJoin, handle, 0, 0).unwrap();
        executor.perform(TaskPrim::Complete, handle, 42, 0).unwrap();
        executor
            .perform(TaskPrim::TakeResult, handle, 0, 0)
            .unwrap();
        assert_eq!(
            executor.perform(TaskPrim::BeginJoin, handle, 0, 0),
            Err(TaskTrap::UnknownHandle)
        );
        assert_eq!(executor.spawned(), 0);
    }

    #[test]
    fn cancellation_reclaims_the_task_and_stales_its_handle() {
        let mut executor = TaskExecutor::new();
        let handle = spawn_one(&mut executor, 1, 7);
        assert_eq!(executor.perform(TaskPrim::Cancel, handle, 0, 0), Ok(0));
        assert_eq!(
            executor.perform(TaskPrim::BeginJoin, handle, 0, 0),
            Err(TaskTrap::UnknownHandle)
        );
        // Nothing claimed it, so no driver would ever pick it up either.
        assert_eq!(executor.perform(TaskPrim::PickReady, 0, 0, 0), Ok(0));
    }

    #[test]
    fn cancelling_a_finished_task_leaves_its_result_joinable() {
        let mut executor = TaskExecutor::new();
        let handle = spawn_one(&mut executor, 1, 7);
        executor.perform(TaskPrim::BeginJoin, handle, 0, 0).unwrap();
        executor.perform(TaskPrim::Complete, handle, 42, 0).unwrap();
        executor.perform(TaskPrim::Cancel, handle, 0, 0).unwrap();
        assert_eq!(executor.perform(TaskPrim::TakeResult, handle, 0, 0), Ok(42));
    }

    #[test]
    fn detach_reclaims_the_task_and_stales_its_handle() {
        let mut executor = TaskExecutor::new();
        let handle = spawn_one(&mut executor, 1, 7);
        assert_eq!(executor.perform(TaskPrim::BeginDetach, handle, 0, 0), Ok(1));
        executor.perform(TaskPrim::Complete, handle, 42, 0).unwrap();
        executor
            .perform(TaskPrim::MarkDetached, handle, 0, 0)
            .unwrap();
        assert_eq!(
            executor.perform(TaskPrim::BeginJoin, handle, 0, 0),
            Err(TaskTrap::UnknownHandle)
        );
        assert_eq!(executor.spawned(), 0);
    }

    #[test]
    fn a_reused_slot_has_a_new_generation() {
        let mut executor = TaskExecutor::new();
        let stale = spawn_one(&mut executor, 1, 7);
        executor.perform(TaskPrim::Cancel, stale, 0, 0).unwrap();
        let current = spawn_one(&mut executor, 2, 9);
        assert_ne!(current, stale);
        assert_eq!(
            executor.perform(TaskPrim::TargetOf, stale, 0, 0),
            Err(TaskTrap::UnknownHandle)
        );
        assert_eq!(executor.perform(TaskPrim::TargetOf, current, 0, 0), Ok(2));
    }

    #[test]
    fn picking_ready_walks_spawn_order_and_skips_claimed_tasks() {
        let mut executor = TaskExecutor::new();
        let first = spawn_one(&mut executor, 1, 1);
        let second = spawn_one(&mut executor, 1, 2);
        assert_eq!(executor.perform(TaskPrim::BeginJoin, second, 0, 0), Ok(1));
        // `second` is running, so the only runnable task left is `first`.
        assert_eq!(executor.perform(TaskPrim::PickReady, 0, 0, 0), Ok(first));
        assert_eq!(executor.perform(TaskPrim::PickReady, 0, 0, 0), Ok(0));
    }

    #[test]
    fn joining_a_running_task_traps_rather_than_recursing() {
        let mut executor = TaskExecutor::new();
        let handle = spawn_one(&mut executor, 1, 7);
        executor.perform(TaskPrim::BeginJoin, handle, 0, 0).unwrap();
        assert_eq!(
            executor.perform(TaskPrim::BeginJoin, handle, 0, 0),
            Err(TaskTrap::Reentrant)
        );
    }

    #[test]
    fn taking_a_result_before_the_body_finishes_traps() {
        let mut executor = TaskExecutor::new();
        let handle = spawn_one(&mut executor, 1, 7);
        assert_eq!(
            executor.perform(TaskPrim::TakeResult, handle, 0, 0),
            Err(TaskTrap::NotFinished)
        );
    }

    #[test]
    fn an_unknown_handle_traps_on_every_primitive_that_takes_one() {
        let mut executor = TaskExecutor::new();
        for prim in [
            TaskPrim::SetArg,
            TaskPrim::SlotGet,
            TaskPrim::TargetOf,
            TaskPrim::BeginJoin,
            TaskPrim::BeginDetach,
            TaskPrim::Complete,
            TaskPrim::TakeResult,
            TaskPrim::MarkDetached,
            TaskPrim::Cancel,
        ] {
            assert_eq!(
                executor.perform(prim, 9, 0, 0),
                Err(TaskTrap::UnknownHandle),
                "{} accepted a handle naming no task",
                prim.label()
            );
        }
        assert_eq!(
            executor.perform(TaskPrim::TargetOf, 0, 0, 0),
            Err(TaskTrap::UnknownHandle)
        );
    }

    #[test]
    fn a_slot_outside_the_fixed_set_traps() {
        let mut executor = TaskExecutor::new();
        let handle = spawn_one(&mut executor, 1, 7);
        assert_eq!(
            executor.perform(TaskPrim::SetArg, handle, TASK_SLOTS as i64, 1),
            Err(TaskTrap::SlotOutOfRange)
        );
        assert_eq!(
            executor.perform(TaskPrim::SlotGet, handle, -1, 0),
            Err(TaskTrap::SlotOutOfRange)
        );
    }

    #[test]
    fn the_clock_only_moves_forward_and_only_when_asked() {
        let mut executor = TaskExecutor::new();
        assert_eq!(executor.clock_ms(), 0);
        executor.perform(TaskPrim::AdvanceClock, 3, 0, 0).unwrap();
        executor.perform(TaskPrim::AdvanceClock, 1, 0, 0).unwrap();
        assert_eq!(executor.clock_ms(), 4);
        // A negative sleep is not a rewind.
        executor.perform(TaskPrim::AdvanceClock, -5, 0, 0).unwrap();
        assert_eq!(executor.clock_ms(), 4);
    }
}
