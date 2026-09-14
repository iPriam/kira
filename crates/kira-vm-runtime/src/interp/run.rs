use super::*;

impl Vm<'_> {
    /// Runs to completion, reclaiming everything still live if it traps.
    ///
    /// A trap leaves live frames and a non-empty operand stack, and both hold
    /// heap storage. Freeing them here is what makes heap accounting mean
    /// something after a failed call: when the heap belongs to one run it is
    /// about to be dropped anyway, but an [`crate::Instance`]'s heap outlives
    /// the call, so a trap that left its frames behind would leak into it.
    pub(super) fn run(&mut self, module: &Module, entry: Frame) -> Result<Value, VmError> {
        self.run_inner(module, entry, None)
    }

    /// Runs with an instruction observer installed.
    pub(super) fn run_with_debug(
        &mut self,
        module: &Module,
        entry: Frame,
        observer: &mut dyn VmDebugObserver,
    ) -> Result<Value, VmError> {
        self.run_inner(module, entry, Some(observer))
    }

    /// Enters the dispatch loop this run needs, and reclaims a trap's storage.
    ///
    /// The loop is selected once, here, rather than tested inside it: observing
    /// instructions and publishing the call stack for a sampler are both
    /// whole-run decisions, and a run that does neither is compiled without
    /// either. Debugging and profiling do not combine — an observer already
    /// sees every frame change as it happens.
    pub(super) fn run_inner(
        &mut self,
        module: &Module,
        entry: Frame,
        observer: Option<&mut dyn VmDebugObserver>,
    ) -> Result<Value, VmError> {
        match self.dispatch_frames(module, Some(entry), observer)? {
            Dispatched::Completed(value) => Ok(value),
            // Only a sliced run can suspend, and a sliced run is entered
            // through `resume`. Reaching here would mean a budget was left on
            // a VM that owns its thread.
            Dispatched::Suspended => Err(VmError::UnexpectedSuspend),
        }
    }

    /// Dispatches `entry` when given one, or continues the frames already on
    /// this VM when resuming a suspended slice.
    pub(super) fn dispatch_frames(
        &mut self,
        module: &Module,
        entry: Option<Frame>,
        observer: Option<&mut dyn VmDebugObserver>,
    ) -> Result<Dispatched, VmError> {
        let mut frames = std::mem::take(&mut self.frames);
        if let Some(entry) = entry {
            frames.push(entry);
        }
        let dispatched = match observer {
            Some(observer) => {
                self.dispatch_inner::<true, false>(module, &mut frames, Some(observer))
            }
            None if crate::profile::enabled() => {
                self.dispatch_inner::<false, true>(module, &mut frames, None)
            }
            None => self.dispatch_inner::<false, false>(module, &mut frames, None),
        };
        let result = match dispatched {
            Ok(outcome) => Ok(outcome),
            Err(error) => {
                self.report_trap_context(&error);
                self.unwind(module, &mut frames);
                Err(error)
            }
        };
        self.frames = frames;
        result
    }

    /// Runs one step the heap owes a parked struct object.
    ///
    /// Entering a body pushes an ordinary frame, so the body is dispatched by
    /// the same loop everything else is and may itself release, call, and park.
    /// The object stays whole until that frame returns, which is what puts the
    /// body *before* the release of everything it holds.
    pub(super) fn run_pending_drop(
        &mut self,
        module: &Module,
        frames: &mut Vec<Frame>,
    ) -> Result<(), VmError> {
        let Some(pending) = self.heap.take_pending_drop() else {
            return Ok(());
        };
        if frames.len() >= MAX_CALL_DEPTH {
            return Err(VmError::CallDepthExceeded);
        }
        let index = u64::from(pending.glue);
        let mut callee = self.take_frame(module, index)?;
        // The receiver is the parked handle itself, not a copy: the body reads
        // the value that is going away. The body's release plan excludes this
        // slot, so the frame it returns from leaves the object for the release
        // below.
        self.stack.push(Value::Struct(pending.id));
        if let Err(error) = self.fill_params(module, index, &mut callee) {
            self.discard(callee.locals);
            return Err(error);
        }
        frames.push(callee);
        // Released once this frame is gone, which is what puts the body before
        // everything the value holds.
        self.running_drops.push((frames.len(), pending.id));
        Ok(())
    }

    /// Releases every parked object whose `Drop` body has finished running.
    ///
    /// A body answers `Void` and nobody asked, so the frame's result is taken
    /// back off the operand stack — the release is not a call any instruction
    /// made, and leaving the unit behind would shift the value the interrupted
    /// instruction was about to read.
    pub(super) fn finish_returned_drops(&mut self, frames: &[Frame]) {
        while let Some(&(depth, id)) = self.running_drops.last() {
            if frames.len() >= depth {
                return;
            }
            self.running_drops.pop();
            // A frame entered above the outermost one pushed its result; the
            // outermost returns its answer instead of pushing it.
            if depth > 1
                && let Some(unit) = self.stack.pop()
            {
                self.heap.drop_value(unit);
            }
            self.heap.finish_pending_drop(id);
        }
    }

    /// Frees the owned locals of every live frame and everything left on the
    /// operand stack.
    ///
    /// Parameter slots are skipped, exactly as the normal-return release plan
    /// skips them ([`kira_ir::mid::scope_releases`] never makes a parameter a
    /// release candidate): a parameter holds a value the caller still owns — a
    /// borrowed `NativeState` owner, most sharply — so releasing it here would
    /// destroy storage the caller keeps and break exactly-once semantics on the
    /// trap path relative to the return path.
    pub(super) fn unwind(&mut self, module: &Module, frames: &mut Vec<Frame>) {
        for mut frame in frames.drain(..) {
            let param_count = module
                .functions
                .get(usize::try_from(frame.func).unwrap_or(usize::MAX))
                .map(|function| usize::try_from(function.param_count).unwrap_or(0))
                .unwrap_or(0);
            let owned = frame.locals.split_off(param_count.min(frame.locals.len()));
            self.discard(owned);
        }
        // Drain in place so a persistent VM can reuse the operand-stack
        // capacity after a trap. `Value` is Copy; the heap drop is the only
        // ownership work needed here.
        for value in self.stack.drain(..) {
            self.heap.drop_value(value);
        }
        // The run is over, so there is nothing left to call a body with. The
        // storage still has to go back.
        self.heap.abandon_pending_drops();
    }
}
