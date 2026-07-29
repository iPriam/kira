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

use kira_bytecode::module::Module;
use kira_runtime_abi::{HostCapabilities, NativeArg, TaskExecutor};

use crate::error::VmError;
use crate::value::{Heap, Value};

use super::Vm;
use super::place::ResolvedStep;

/// One call frame: its function, program counter, and local slots.
pub(super) struct Frame {
    pub(super) func: u32,
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
}

/// A resolved writeback target on a callee frame.
pub(super) struct Writeback {
    /// The callee-frame local slot — a parameter — whose value is moved out.
    pub(super) param: u16,
    /// The caller-frame local slot the place is rooted at.
    pub(super) slot: u16,
    /// The steps to walk into the caller's storage, indices already resolved;
    /// empty writes the caller's local slot itself.
    pub(super) steps: Vec<ResolvedStep>,
}

/// A fresh frame for `index`, with every local slot at `Void`.
pub(super) fn new_frame(module: &Module, index: u32) -> Result<Frame, VmError> {
    let function = module
        .functions
        .get(index as usize)
        .ok_or(VmError::UnknownFunction(index))?;
    Ok(Frame {
        func: index,
        pc: 0,
        locals: vec![Value::Void; function.local_count as usize],
        writebacks: Vec::new(),
    })
}

impl<'h> Vm<'h> {
    /// A VM that runs on `heap` and reaches the world through `host`.
    ///
    /// The heap is taken rather than created here because it does not always
    /// belong to one run: [`crate::Instance`] lends the VM a heap that outlives
    /// the call and takes it back afterwards.
    pub(crate) fn new(host: &'h mut dyn HostCapabilities, heap: Heap) -> Self {
        Vm {
            host,
            heap,
            stack: Vec::new(),
            steps: Vec::new(),
            native_path: Vec::new(),
            tasks: TaskExecutor::new(),
        }
    }

    /// Gives the heap back, whatever the run did with it.
    pub(crate) fn into_heap(self) -> Heap {
        self.heap
    }

    /// Runs `function_id` with `args` in its parameter slots, to completion.
    ///
    /// Arguments are lowered into this run's own heap, so the caller's storage
    /// is only read: a `&str` argument is copied in rather than aliased.
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
    pub(crate) fn enter_values(
        &mut self,
        module: &Module,
        function_id: u32,
        args: Vec<Value>,
    ) -> Result<Value, VmError> {
        let mut frame = match new_frame(module, function_id) {
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
                function: function_id,
                expected: module.functions[function_id as usize].param_count,
                got,
            });
        }
        for (slot, value) in args.into_iter().enumerate() {
            frame.locals[slot] = value;
        }
        self.run(module, frame)
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
    pub(super) fn fill_params(
        &mut self,
        module: &Module,
        index: u32,
        frame: &mut Frame,
    ) -> Result<(), VmError> {
        let param_count = module.functions[index as usize].param_count as usize;
        for slot in (0..param_count).rev() {
            frame.locals[slot] = self.pop()?;
        }
        Ok(())
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
            let previous = std::mem::replace(&mut caller.locals[writeback.slot as usize], value);
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
