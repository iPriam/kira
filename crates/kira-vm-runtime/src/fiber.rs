//! A suspended Kira execution with its own heap, stack, and locals.
//!
//! A `@MainThreadLifecycle` function is a long-lived loop that shares the
//! process main thread with its siblings and with dispatched `@MainThread`
//! work. Waiting on one would freeze the others, so each runs in slices: the
//! interpreter stops on an instruction boundary and this type keeps everything
//! the next slice needs, which is what lets a lifecycle keep its locals across
//! turns rather than restarting.

use kira_bytecode::module::Module;
use kira_runtime_abi::HostCapabilities;

use crate::error::VmError;
use crate::interp::{Dispatched, Vm, VmExecutors, VmScratch};
use crate::value::Heap;

/// How far a fiber got in one slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FiberStep {
    /// The budget ran out with the loop still live.
    Suspended,
    /// The function returned; this fiber is finished.
    Finished,
}

/// One lifecycle's execution, resumable across slices.
pub struct Fiber {
    /// The function this fiber runs.
    function: u32,
    /// This fiber's own heap. Nothing is shared with another fiber or with the
    /// application thread, so no lock on this path can be held by a stalled
    /// thread.
    heap: Heap,
    /// The operand stack, frame stack, and constants between slices.
    scratch: VmScratch,
    /// The task and channel tables between slices.
    ///
    /// A lifecycle is one execution however many slices it takes, so the rows
    /// it created are its own for as long as it runs. Rebuilding them per slice
    /// would break every handle the loop still holds at the boundary the
    /// scheduler chose, which is not a boundary the program can see.
    executors: VmExecutors,
    /// Whether the entry frame has been pushed.
    started: bool,
    /// Whether the function has returned.
    finished: bool,
}

impl Fiber {
    /// Creates a fiber that will run `function` when first stepped.
    #[must_use]
    pub fn new(function: u32) -> Self {
        Fiber {
            function,
            heap: Heap::new(),
            scratch: VmScratch::default(),
            executors: VmExecutors::default(),
            started: false,
            finished: false,
        }
    }

    /// Whether this fiber has run to completion.
    #[must_use]
    pub fn finished(&self) -> bool {
        self.finished
    }

    /// Runs up to `budget` instructions, then reports where the fiber stopped.
    ///
    /// The host is rebound each slice because it belongs to the thread rather
    /// than to the fiber; everything the fiber owns travels in `self`.
    pub fn step(
        &mut self,
        host: &mut dyn HostCapabilities,
        module: &Module,
        budget: u64,
    ) -> Result<FiberStep, VmError> {
        if self.finished {
            return Ok(FiberStep::Finished);
        }
        let scratch = std::mem::take(&mut self.scratch);
        let mut vm = Vm::new_with_scratch(host, std::mem::take(&mut self.heap), scratch);
        vm.adopt_executors(std::mem::take(&mut self.executors));
        vm.set_slice_budget(Some(budget));
        let outcome = if self.started {
            vm.resume_sliced(module)
        } else {
            self.started = true;
            vm.enter_sliced(module, self.function)
        };
        // The heap comes back on every path, including a trap: a fiber that
        // failed still owns storage the caller has to be able to account for.
        let finished = matches!(outcome, Ok(Dispatched::Completed(_)));
        if finished {
            vm.release_constants();
        }
        self.executors = vm.take_executors();
        let (heap, scratch) = vm.into_heap_and_scratch();
        self.heap = heap;
        self.scratch = scratch;
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(error) => {
                // A trapped fiber is over. Leaving it unfinished would have the
                // scheduler re-enter a dead loop on every pass.
                self.finished = true;
                return Err(error);
            }
        };
        match outcome {
            Dispatched::Completed(value) => {
                self.heap.drop_value(value);
                self.finished = true;
                Ok(FiberStep::Finished)
            }
            Dispatched::Suspended => Ok(FiberStep::Suspended),
        }
    }
}
