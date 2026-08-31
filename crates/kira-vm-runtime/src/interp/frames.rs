//! Call frames: entering a function, filling its parameter slots, and writing
//! its written-through parameters back into the caller.
//!
//! Split from the dispatch loop on the file-size ladder. What is here is the
//! *shape* of a call — how a frame comes into existence, what it owes the
//! caller when it returns, and who frees what on every path out. The loop that
//! executes instructions between those two moments stays in [`super`].
//!
//! Ownership is the reason this is one module rather than three helpers. Every
//! path here either moves a value into a slot or frees it, and the cases that
//! look like edges — a refused call, a mid-fill failure, a malformed writeback
//! place — are the ones where a leak or a double free would hide. Keeping them
//! together is what makes that reviewable.

use kira_bytecode::module::{FrameRelease, Module};
use kira_runtime_abi::{HostCapabilities, NativeArg, TaskExecutor};

use crate::debug::VmDebugObserver;
use crate::error::VmError;
use crate::value::{Heap, Value};

use super::place::ResolvedStep;
use super::{Vm, VmScratch};

/// How many returned frames the per-run cache retains.
///
/// A cache as deep as the deepest call ever made would make a long-lived
/// instance pay for its worst transient forever: one call recursing near the
/// depth guard would strand ~10⁶ frames. Sixty-four covers the call shapes a
/// typical program re-enters allocation-free while bounding retention to a
/// fixed handful of frames.
const FRAME_CACHE_LIMIT: usize = 64;

/// One call frame: its function, program counter, and local slots.
pub(super) struct Frame {
    pub(super) func: u64,
    pub(super) pc: usize,
    pub(super) locals: Vec<Value>,
    /// Which of this frame's final parameter slots are written back on return.
    ///
    /// Non-empty only for a frame entered by
    /// [`kira_bytecode::op::Instruction::CallMut`] — a mutating method's
    /// receiver — or [`kira_bytecode::op::Instruction::CallWriteback`], which
    /// names any set of parameters. On return each named slot moves into its
    /// caller place before the callee's locals are dropped, which is the whole
    /// of value-semantics writeback. Empty for every ordinary call.
    pub(super) writebacks: Vec<Writeback>,
    /// Which parameter slots this frame hands back when it is the outermost.
    ///
    /// Non-empty only for a frame an embedder entered directly and asked to
    /// report written-through parameters — the native half calling a `@Runtime`
    /// function that takes a `borrow mut`. There is no caller frame to write
    /// into: the caller is the other engine, so the values are moved out here
    /// and handed to whoever started the call.
    pub(super) capture: Vec<u32>,
}

/// A resolved writeback target on a callee frame.
pub(super) struct Writeback {
    /// The callee-frame local slot — a parameter — whose value is moved out.
    pub(super) param: u64,
    /// The caller-frame local slot the place is rooted at.
    pub(super) slot: u64,
    /// The steps to walk into the caller's storage, indices already resolved;
    /// empty writes the caller's local slot itself.
    pub(super) steps: Vec<ResolvedStep>,
}

/// A fresh frame for `index`, with every local slot at `Void`.
fn fresh_frame(module: &Module, index: u64) -> Result<Frame, VmError> {
    let function = module
        .functions
        .get(usize::try_from(index).map_err(|_| VmError::UnknownFunction(index))?)
        .ok_or(VmError::UnknownFunction(index))?;
    let local_count = usize::try_from(function.local_count)
        .map_err(|_| VmError::LocalSlotOutOfRange(function.local_count))?;
    Ok(Frame {
        func: index,
        pc: 0,
        locals: vec![Value::Void; local_count],
        writebacks: Vec::new(),
        capture: Vec::new(),
    })
}

impl<'h> Vm<'h> {
    /// A VM that runs on `heap` and reaches the world through `host`.
    ///
    /// The heap is taken rather than created here because it does not always
    /// belong to one run: [`crate::Instance`] lends the VM a heap that outlives
    /// the call and takes it back afterwards.
    pub(crate) fn new(host: &'h mut dyn HostCapabilities, heap: Heap) -> Self {
        Self::new_with_scratch(host, heap, VmScratch::default())
    }

    /// Creates a VM with reusable storage returned by a previous call.
    pub(crate) fn new_with_scratch(
        host: &'h mut dyn HostCapabilities,
        heap: Heap,
        scratch: VmScratch,
    ) -> Self {
        Vm {
            host,
            heap,
            stack: scratch.stack,
            frames: scratch.frames,
            steps: scratch.steps,
            native_path: scratch.native_path,
            string_args: scratch.string_args,
            pending_capture: scratch.pending_capture,
            captured: scratch.captured,
            tasks: TaskExecutor::new(),
            frame_cache: scratch.frame_cache,
            native_writebacks: scratch.native_writebacks,
            native_scratch: scratch.native_scratch,
            constants: scratch.constants,
            initializing_constants: false,
            running_drops: Vec::new(),
            trap_probe: None,
            slice_budget: None,
        }
    }

    /// Returns a finished frame to the per-run cache, up to
    /// [`FRAME_CACHE_LIMIT`].
    ///
    /// Past the limit the frame is dropped: retention keyed to the deepest
    /// call ever made would otherwise grow without bound on a persistent
    /// [`crate::Instance`].
    fn cache_frame(&mut self, frame: Frame) {
        if self.frame_cache.len() < FRAME_CACHE_LIMIT {
            self.frame_cache.push(frame);
        }
    }

    /// Takes a frame from the per-run cache, growing its locals only when a
    /// deeper call shape needs more capacity than any frame returned so far.
    ///
    /// The cache contains only frames whose heap-bearing locals have already
    /// been released. Clearing the vector therefore drops no runtime object;
    /// it only resets the copied scalar state before the frame is retargeted.
    pub(super) fn take_frame(&mut self, module: &Module, index: u64) -> Result<Frame, VmError> {
        let function = module
            .functions
            .get(usize::try_from(index).map_err(|_| VmError::UnknownFunction(index))?)
            .ok_or(VmError::UnknownFunction(index))?;
        let mut frame = match self.frame_cache.pop() {
            Some(frame) => frame,
            None => fresh_frame(module, index)?,
        };
        frame.func = index;
        frame.pc = 0;
        let local_count = usize::try_from(function.local_count)
            .map_err(|_| VmError::LocalSlotOutOfRange(function.local_count))?;
        if frame.locals.len() != local_count {
            // Cached frames contain only released heap values. Clearing before
            // resizing is therefore just a scalar reset, not an ownership
            // operation; the equal-sized fast path below is already all Void.
            frame.locals.clear();
            frame.locals.resize(local_count, Value::Void);
        }
        frame.writebacks.clear();
        frame.capture.clear();
        Ok(frame)
    }

    /// Returns the persistent heap and reusable non-task storage after a call.
    pub(crate) fn into_heap_and_scratch(self) -> (Heap, VmScratch) {
        let Vm {
            heap,
            stack,
            frames,
            steps,
            native_path,
            string_args,
            pending_capture,
            captured,
            frame_cache,
            native_writebacks,
            native_scratch,
            constants,
            ..
        } = self;
        (
            heap,
            VmScratch {
                stack,
                frames,
                steps,
                native_path,
                string_args,
                pending_capture,
                captured,
                frame_cache,
                native_writebacks,
                native_scratch,
                constants,
            },
        )
    }

    /// Runs `function_id` with `args` in its parameter slots, to completion.
    ///
    /// Arguments are lowered into this run's own heap, so the caller's storage
    /// is only read: a `&str` argument is copied in rather than aliased.
    /// [`Vm::enter`], also handing back the final value of each slot in
    /// `capture`.
    ///
    /// The written-through parameters of a call that came from the other
    /// engine. They are moved out of the entry frame as it returns, so what
    /// comes back is owned by this heap exactly as the result is.
    pub(super) fn enter_capturing(
        &mut self,
        module: &Module,
        function_id: u32,
        args: &[NativeArg<'_>],
        capture: &[u32],
    ) -> Result<(Value, Vec<(u32, Value)>), VmError> {
        self.pending_capture = capture.to_vec();
        let result = self.enter(module, function_id, args);
        let captured = std::mem::take(&mut self.captured);
        match result {
            Ok(value) => Ok((value, captured)),
            Err(error) => {
                self.discard(captured.into_iter().map(|(_, value)| value));
                Err(error)
            }
        }
    }

    pub(super) fn enter(
        &mut self,
        module: &Module,
        function_id: u32,
        args: &[NativeArg<'_>],
    ) -> Result<Value, VmError> {
        let mut lowered = Vec::with_capacity(args.len());
        for argument in args {
            match self.heap.lower(*argument) {
                Some(value) => lowered.push(value),
                None => {
                    // The arguments already lowered are this heap's; a refused
                    // call frees them rather than leaving them behind.
                    self.discard(lowered);
                    return Err(VmError::HandleAtSeam {
                        function: function_id,
                    });
                }
            }
        }
        self.enter_values(module, function_id, lowered)
    }

    /// Runs `function_id` with values already lowered into this VM's heap.
    ///
    /// Takes ownership of `args`: every one of them is either moved into a
    /// parameter slot — and dropped with the frame — or freed here, on every
    /// path out, including the ones that never start the function.
    /// Sets how many instructions the next slice may run before suspending.
    pub(crate) fn set_slice_budget(&mut self, budget: Option<u64>) {
        self.slice_budget = budget;
    }

    /// Enters `function_id` with no arguments under the current slice budget.
    ///
    /// The lifecycle entry: a `@MainThreadLifecycle` function takes no
    /// parameters, so there is nothing to marshal, and the run may suspend
    /// before it returns.
    pub(crate) fn enter_sliced(
        &mut self,
        module: &Module,
        function_id: u32,
    ) -> Result<super::Dispatched, VmError> {
        self.ensure_constants(module)?;
        let frame = self.take_frame(module, u64::from(function_id))?;
        self.dispatch_frames(module, Some(frame), None)
    }

    /// Continues the frames a previous slice suspended on.
    pub(crate) fn resume_sliced(&mut self, module: &Module) -> Result<super::Dispatched, VmError> {
        self.dispatch_frames(module, None, None)
    }

    pub(crate) fn enter_values(
        &mut self,
        module: &Module,
        function_id: u32,
        args: Vec<Value>,
    ) -> Result<Value, VmError> {
        if let Err(error) = self.ensure_constants(module) {
            self.discard(args);
            return Err(error);
        }
        let mut frame = match self.take_frame(module, u64::from(function_id)) {
            Ok(frame) => frame,
            Err(error) => {
                self.discard(args);
                return Err(error);
            }
        };
        if args.len() > frame.locals.len() {
            // Validation proves `param_count <= local_count` and the entry
            // points check arity, so this is unreachable through either door —
            // it is here so the impossible case frees rather than panics. The
            // count is read before the values are freed, so the refusal names
            // what actually arrived rather than a placeholder.
            let got = args.len();
            self.discard(args);
            self.discard(frame.locals);
            return Err(VmError::ArityMismatch {
                function: u64::from(function_id),
                expected: module.functions[usize::try_from(function_id)
                    .map_err(|_| VmError::UnknownFunction(u64::from(function_id)))?]
                .param_count,
                got,
            });
        }
        for (slot, value) in args.into_iter().enumerate() {
            frame.locals[slot] = value;
        }
        // Only the frame the embedder entered captures: a nested call's
        // writebacks go to its caller's places, which is a different mechanism
        // and already handled. Taking it here is what keeps it to one frame.
        frame.capture = std::mem::take(&mut self.pending_capture);
        self.run(module, frame)
    }

    /// Runs `function_id` with an instruction observer installed.
    pub(crate) fn enter_values_with_debug(
        &mut self,
        module: &Module,
        function_id: u32,
        args: Vec<Value>,
        observer: &mut dyn VmDebugObserver,
    ) -> Result<Value, VmError> {
        if let Err(error) = self.ensure_constants(module) {
            self.discard(args);
            return Err(error);
        }
        let mut frame = match self.take_frame(module, u64::from(function_id)) {
            Ok(frame) => frame,
            Err(error) => {
                self.discard(args);
                return Err(error);
            }
        };
        if args.len() > frame.locals.len() {
            let got = args.len();
            self.discard(args);
            self.discard(frame.locals);
            return Err(VmError::ArityMismatch {
                function: u64::from(function_id),
                expected: module.functions[usize::try_from(function_id)
                    .map_err(|_| VmError::UnknownFunction(u64::from(function_id)))?]
                .param_count,
                got,
            });
        }
        for (slot, value) in args.into_iter().enumerate() {
            frame.locals[slot] = value;
        }
        frame.capture = std::mem::take(&mut self.pending_capture);
        self.run_with_debug(module, frame, observer)
    }

    /// Fills every module-constant slot this VM has not filled yet.
    ///
    /// Each slot is computed by one call of its init function, front to back
    /// in the module's table order — the compiler's dependency order, so a
    /// later init's `LoadConstant` of an earlier slot always finds it. Runs
    /// before an embedder entry's first frame; the init calls re-enter
    /// [`Vm::enter_values`], and the guard flag is what keeps that re-entry
    /// from starting the fill again.
    fn ensure_constants(&mut self, module: &Module) -> Result<(), VmError> {
        if self.initializing_constants || self.constants.len() >= module.constants.len() {
            return Ok(());
        }
        // Constants are program-start work, not part of any slice: their
        // initializers run through `enter_values`, which cannot suspend, so a
        // budget installed for a sliced entry must not count them — a heavy
        // initializer exhausting it would trap the run instead of yielding.
        let budget = self.slice_budget.take();
        self.initializing_constants = true;
        let result = self.fill_constants(module);
        self.initializing_constants = false;
        self.slice_budget = budget;
        result
    }

    /// Runs each unfilled module-constant initializer, in slot order.
    fn fill_constants(&mut self, module: &Module) -> Result<(), VmError> {
        while self.constants.len() < module.constants.len() {
            let init = module.constants[self.constants.len()];
            let Ok(init) = u32::try_from(init) else {
                return Err(VmError::UnknownFunction(init));
            };
            let value = self.enter_values(module, init, Vec::new())?;
            self.constants.push(value);
        }
        Ok(())
    }

    /// Frees every filled module-constant slot.
    ///
    /// A one-shot run calls this before reading heap accounting, so a clean
    /// program still reports `current == 0`; a per-call VM on a shared heap
    /// calls it before handing the heap back, so constants do not accumulate
    /// across crossings.
    pub(crate) fn release_constants(&mut self) {
        let values = std::mem::take(&mut self.constants);
        self.discard(values);
    }

    /// Frees a batch of values this VM owns.
    pub(super) fn discard(&mut self, values: impl IntoIterator<Item = Value>) {
        for value in values {
            self.heap.drop_value(value);
        }
    }

    /// Pops arguments off the operand stack into a fresh callee frame's
    /// parameter slots (arguments were pushed left to right).
    ///
    /// Fills in place rather than taking the frame, so a mid-fill failure hands
    /// the caller back a frame holding the slots already written — which it must
    /// free, because a partially filled frame never reaches the frame stack the
    /// unwind walks.
    #[inline(always)]
    pub(super) fn fill_params(
        &mut self,
        module: &Module,
        index: u64,
        frame: &mut Frame,
    ) -> Result<(), VmError> {
        let param_count = usize::try_from(
            module
                .functions
                .get(usize::try_from(index).map_err(|_| VmError::UnknownFunction(index))?)
                .ok_or(VmError::UnknownFunction(index))?
                .param_count,
        )
        .map_err(|_| VmError::LocalSlotOutOfRange(index))?;
        if param_count == 0 {
            return Ok(());
        }
        if param_count == 1 {
            frame.locals[0] = self.pop()?;
            return Ok(());
        }
        for slot in (0..param_count).rev() {
            frame.locals[slot] = self.pop()?;
        }
        Ok(())
    }

    /// Tears down the frame that just returned, and answers with the run's
    /// value when it was the last one.
    ///
    /// Two steps in this order and no other: the written-through parameters
    /// move into their caller's places, then the slots the function's release
    /// plan names are freed. A parameter that moved out left `Value::Void`
    /// behind, so a plan naming it frees nothing the caller now holds.
    ///
    /// **What is released is not decided here.** `kira_ir::mid` decides it, for
    /// this engine and the native one alike, and the compiler writes its answer
    /// into the module as [`FrameRelease::Planned`]. This walks that answer. A
    /// module carrying no plan — one built by hand, or written before the
    /// section existed — asks for [`FrameRelease::EveryLocal`] instead, which is
    /// the discipline the VM had before plans existed: safe on any module,
    /// because a slot the function does not own holds a scalar or an opaque
    /// token and freeing either does nothing.
    pub(super) fn finish_frame(
        &mut self,
        module: &Module,
        frames: &mut Vec<Frame>,
        result: Value,
    ) -> Result<Option<Value>, VmError> {
        let Some(mut finished) = frames.pop() else {
            return Err(VmError::FrameUnderflow);
        };
        let Some(function) = usize::try_from(finished.func)
            .ok()
            .and_then(|index| module.functions.get(index))
        else {
            self.discard(finished.locals);
            self.heap.drop_value(result);
            return Err(VmError::UnknownFunction(finished.func));
        };
        // An `EveryLocal` frame with no written-through parameters — a module
        // built by hand, or written before release plans existed — has no
        // writeback or capture work to do, so keep this branch ahead of the
        // general plan walker. It also avoids taking the empty capture vector
        // and matching the release-plan variants on that path.
        if finished.writebacks.is_empty()
            && finished.capture.is_empty()
            && matches!(&function.releases, FrameRelease::EveryLocal)
        {
            for held in &mut finished.locals {
                let value = std::mem::replace(held, Value::Void);
                if is_heap_value(&value) {
                    self.heap.drop_value(value);
                }
            }
            self.cache_frame(finished);
            if frames.is_empty() {
                // Structural validation cannot prove operand-stack typing. A
                // hand-built module may return while leaving an extra value on
                // the stack; reclaim it before a persistent Instance lends
                // this VM's scratch storage to another call.
                for value in self.stack.drain(..) {
                    self.heap.drop_value(value);
                }
                return Ok(Some(result));
            }
            self.stack.push(result);
            return Ok(None);
        }
        // Every target names a distinct parameter, so taking each in turn
        // leaves the rest intact.
        if !finished.writebacks.is_empty() {
            // Keep the outer vector attached to the frame. `take_frame`
            // clears it for the next call, so retaining its capacity removes
            // one allocation from every repeated mutable call. The individual
            // paths remain live until the writebacks have landed, then are
            // cleared for reuse too.
            let mut writebacks = std::mem::take(&mut finished.writebacks);
            for writeback in &mut writebacks {
                let Some(value) = finished
                    .locals
                    .get_mut(match usize::try_from(writeback.param) {
                        Ok(index) => index,
                        Err(_) => continue,
                    })
                    .map(|slot| std::mem::replace(slot, Value::Void))
                else {
                    continue;
                };
                if let Err(error) = self.write_back(frames, writeback, value) {
                    self.discard(finished.locals);
                    self.heap.drop_value(result);
                    return Err(error);
                }
            }
            for writeback in &mut writebacks {
                writeback.steps.clear();
            }
            finished.writebacks = writebacks;
        }
        // Taken before the release walk, for the same reason a writeback is:
        // a captured slot has moved out, so the plan that names it frees the
        // `Void` left behind rather than the value now in the embedder's hands.
        let capture = std::mem::take(&mut finished.capture);
        for slot in capture {
            let Some(value) = finished
                .locals
                .get_mut(match usize::try_from(slot) {
                    Ok(index) => index,
                    Err(_) => continue,
                })
                .map(|held| std::mem::replace(held, Value::Void))
            else {
                continue;
            };
            self.captured.push((slot, value));
        }
        let (cacheable, reset_locals) = match &function.releases {
            FrameRelease::EveryLocal => {
                for held in &mut finished.locals {
                    let value = std::mem::replace(held, Value::Void);
                    if is_heap_value(&value) {
                        self.heap.drop_value(value);
                    }
                }
                // Every slot was just replaced with Void, so there is no
                // second scan to perform before caching this frame.
                (true, false)
            }
            FrameRelease::Planned(slots) => {
                for &slot in slots {
                    // A slot past the frame is refused by `Module::validate`
                    // before a module runs; skipping it here is what makes the
                    // unreachable case a leak rather than a panic.
                    let Some(held) = finished
                        .locals
                        .get_mut(match usize::try_from(slot) {
                            Ok(index) => index,
                            Err(_) => continue,
                        })
                        .map(|slot| std::mem::replace(slot, Value::Void))
                    else {
                        continue;
                    };
                    if is_heap_value(&held) {
                        self.heap.drop_value(held);
                    }
                }
                // A planned module is allowed to leave scalar or opaque
                // values in slots it does not own. Those are safe to retain;
                // an unplanned heap handle is not.
                (
                    finished.locals.iter().all(|value| !is_heap_value(value)),
                    true,
                )
            }
        };
        // A planned release names every heap-bearing local. Scalar values and
        // opaque seam words can be reset in place, so only a frame with no
        // remaining heap handle is safe to retain. If a malformed or older
        // module leaves one behind, leave this frame out of the cache rather
        // than making reuse observable.
        if cacheable {
            if reset_locals {
                finished.locals.fill(Value::Void);
            }
            self.cache_frame(finished);
        }
        if frames.is_empty() {
            // Structural validation cannot prove operand-stack typing. A
            // hand-built module may return while leaving an extra value on the
            // stack; reclaim it before a persistent Instance lends this VM's
            // scratch storage to another call.
            for value in self.stack.drain(..) {
                self.heap.drop_value(value);
            }
            return Ok(Some(result));
        }
        self.stack.push(result);
        Ok(None)
    }

    /// Writes one of a returning frame's final parameter slots back into the
    /// caller's place — the whole of value-semantics writeback.
    ///
    /// The caller is the frame just beneath the one that returned, always
    /// present because a frame carrying writebacks is only ever entered by
    /// `CallMut` or `CallWriteback` from a caller. An empty path overwrites the
    /// caller's local itself (`g.mutate()`); a non-empty one walks into it,
    /// exactly as [`Vm::store_place`] does. The value overwritten is dropped;
    /// `value` is moved into the place, so it is not double-freed with the
    /// callee's other locals. The `slot` was bounds-checked against the caller's
    /// function by [`Module::validate`], so the direct index matches the
    /// discipline every other place walk follows.
    pub(super) fn write_back(
        &mut self,
        frames: &mut [Frame],
        writeback: &Writeback,
        value: Value,
    ) -> Result<(), VmError> {
        let Some(caller) = frames.last_mut() else {
            self.heap.drop_value(value);
            return Err(VmError::FrameUnderflow);
        };
        if writeback.steps.is_empty() {
            let Ok(slot) = usize::try_from(writeback.slot) else {
                self.heap.drop_value(value);
                return Err(VmError::LocalSlotOutOfRange(writeback.slot));
            };
            let Some(previous) = caller
                .locals
                .get_mut(slot)
                .map(|local| std::mem::replace(local, value))
            else {
                self.heap.drop_value(value);
                return Err(VmError::LocalSlotOutOfRange(writeback.slot));
            };
            self.heap.drop_value(previous);
            return Ok(());
        }
        match self.store_place(caller, writeback.slot, &writeback.steps, value) {
            Ok(()) => Ok(()),
            Err(error) => {
                // A malformed place — unreachable past validation and typing —
                // leaves the value unstored, so it is freed rather than
                // leaked into a heap whose accounting a trap still checks.
                self.heap.drop_value(value);
                Err(error)
            }
        }
    }
}

/// Whether dropping `value` releases an object in this VM's heap.
fn is_heap_value(value: &Value) -> bool {
    matches!(
        value,
        Value::Str(_)
            | Value::Struct(_)
            | Value::Array(_)
            | Value::Enum(_)
            | Value::Erased(_)
            | Value::Cell(_)
            | Value::NativeSnapshot(_)
            | Value::CBlock(_)
            | Value::MainThreadTask(_)
    )
}
