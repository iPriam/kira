//! The bytecode interpreter: a match-in-loop stack machine.
//!
//! The interpreter keeps call frames on a heap-allocated stack (so Kira
//! recursion never consumes the host's native stack) and a single shared
//! operand stack. It touches the outside world only through the
//! [`HostCapabilities`] trait, so the whole crate stays portable to
//! `wasm32-unknown-unknown`.

use kira_bytecode::module::Module;
use kira_bytecode::op::Instruction;
use kira_runtime_abi::{
    HostCapabilities, NativeStatePathStep, NativeStateToken, NativeStateTypeId, NativeStateValue,
    TaskExecutor,
};

use crate::debug::{VmDebugAction, VmDebugEvent, VmDebugFrame, VmDebugObserver};
use crate::error::VmError;
use crate::value::{Heap, Value};

mod arrays;
mod cells;
mod compiler;
mod env;
mod file_system;
mod frames;
mod host;
mod instructions;
mod native_state;
mod operators;
mod place;
mod program;
mod strings;

pub(crate) use self::program::check_signature;
pub use self::program::{Program, RunOutcome, execute, execute_with_debug};

use self::frames::{Frame, Writeback};
use self::place::ResolvedStep;

/// Guards against unbounded recursion turning into unbounded memory use.
const MAX_CALL_DEPTH: usize = 1 << 20;

/// The running interpreter: a host, a heap, an operand stack, and scratch.
pub(crate) struct Vm<'h> {
    host: &'h mut dyn HostCapabilities,
    pub(crate) heap: Heap,
    stack: Vec<Value>,
    /// Reusable call-stack storage. Taking this vector for one run preserves
    /// its capacity across calls on a persistent [`crate::Instance`] without
    /// borrowing the VM and the dispatch loop at the same time.
    frames: Vec<Frame>,
    /// Reusable scratch for a dynamic place's resolved steps.
    ///
    /// A `StorePlace`/`ArrayAppend` resolves its path into this buffer once per
    /// execution; keeping it on the VM and reusing its capacity is what keeps
    /// those ops off the per-op allocation the interpreter's hot loop forbids.
    /// It is taken out with `mem::take` while filled — so the fill can pop the
    /// operand stack without borrowing the VM twice — then handed back cleared,
    /// never freed.
    steps: Vec<ResolvedStep>,
    /// Reusable scratch for a path into callback state.
    ///
    /// Same reason as `steps`: a write through a recovered view happens on every
    /// mutation of native state, and allocating its path each time would put an
    /// allocation on that path.
    native_path: Vec<NativeStatePathStep>,
    /// Reusable scratch for arguments of a shared `StringOp` instruction.
    ///
    /// Taking this buffer out around the pop/execute sequence keeps the normal
    /// string-operation path allocation-free while preserving ownership
    /// cleanup when malformed bytecode traps during argument collection.
    string_args: Vec<Value>,
    /// Slots the next entered frame should hand back.
    ///
    /// Set immediately before entering and taken by the frame that starts, so
    /// it names the outermost frame and no other.
    pending_capture: Vec<u16>,
    /// The final values of the parameters an embedder asked to have back.
    ///
    /// Filled by the outermost frame as it returns, and only when the embedder
    /// named slots to capture — the native half calling a `@Runtime` function
    /// that writes through a parameter. Empty for every other run.
    captured: Vec<(u32, Value)>,
    /// The deferred tasks this run spawned.
    ///
    /// One table per run, because a handle is an index into it: two runs
    /// sharing a table would let one program's handle name another's task. The
    /// *policy* is not here — the scheduler is generated Kira the IR
    /// synthesizes, so this only answers what each primitive means.
    tasks: TaskExecutor,
    /// Returned call frames whose local-vector capacity can be reused by the
    /// next call. Recursive programs otherwise allocate one `Vec<Value>` for
    /// every invocation even when the same small set of local shapes repeats.
    frame_cache: Vec<Frame>,
    /// Reusable writeback descriptors for calls into a native half.
    ///
    /// Native writebacks do not need to survive a frame, but their resolved
    /// paths can be just as allocation-heavy as VM-to-VM writebacks. Keep the
    /// descriptor vector and each path's capacity on the VM while the native
    /// call is in progress, then return it cleared for the next crossing.
    native_writebacks: Vec<Writeback>,
    /// Temporary buffers used to marshal the next native crossing.
    native_scratch: NativeCallScratch,
}

/// Temporary native-call storage detached from [`Vm`] while a host call runs.
///
/// The argument trees borrow the VM heap and the lowered arguments borrow those
/// trees, so keeping these vectors together makes their lifetime relationship
/// explicit while still allowing every crossing to reuse its allocations.
#[derive(Default)]
pub(crate) struct NativeCallScratch {
    pub(crate) arguments: Vec<Value>,
    pub(crate) trees: Vec<Option<NativeStateValue>>,
    pub(crate) native_views: Vec<Option<(NativeStateToken, NativeStateTypeId)>>,
}

impl NativeCallScratch {
    pub(crate) fn clear(&mut self) {
        self.arguments.clear();
        self.trees.clear();
        self.native_views.clear();
    }
}

/// Reusable interpreter storage that can outlive one call on a persistent
/// [`crate::Instance`]. The task table is intentionally absent: task handles
/// are valid only for one run and are recreated for every entry.
#[derive(Default)]
pub(crate) struct VmScratch {
    stack: Vec<Value>,
    frames: Vec<Frame>,
    steps: Vec<ResolvedStep>,
    native_path: Vec<NativeStatePathStep>,
    string_args: Vec<Value>,
    pending_capture: Vec<u16>,
    captured: Vec<(u32, Value)>,
    frame_cache: Vec<Frame>,
    native_writebacks: Vec<Writeback>,
    native_scratch: NativeCallScratch,
}

impl Vm<'_> {
    /// Runs to completion, reclaiming everything still live if it traps.
    ///
    /// A trap leaves live frames and a non-empty operand stack, and both hold
    /// heap storage. Freeing them here is what makes heap accounting mean
    /// something after a failed call: when the heap belongs to one run it is
    /// about to be dropped anyway, but an [`crate::Instance`]'s heap outlives
    /// the call, so a trap that left its frames behind would leak into it.
    fn run(&mut self, module: &Module, entry: Frame) -> Result<Value, VmError> {
        self.run_inner::<false>(module, entry, None)
    }

    /// Runs with an instruction observer installed.
    fn run_with_debug(
        &mut self,
        module: &Module,
        entry: Frame,
        observer: &mut dyn VmDebugObserver,
    ) -> Result<Value, VmError> {
        self.run_inner::<true>(module, entry, Some(observer))
    }

    fn run_inner<const DEBUG: bool>(
        &mut self,
        module: &Module,
        entry: Frame,
        observer: Option<&mut dyn VmDebugObserver>,
    ) -> Result<Value, VmError> {
        let mut frames = std::mem::take(&mut self.frames);
        frames.push(entry);
        let result = match self.dispatch_inner::<DEBUG>(module, &mut frames, observer) {
            Ok(value) => Ok(value),
            Err(error) => {
                self.unwind(&mut frames);
                Err(error)
            }
        };
        self.frames = frames;
        result
    }

    /// Frees every local of every live frame and everything left on the operand
    /// stack.
    fn unwind(&mut self, frames: &mut Vec<Frame>) {
        for frame in frames.drain(..) {
            self.discard(frame.locals);
        }
        // Drain in place so a persistent VM can reuse the operand-stack
        // capacity after a trap. `Value` is Copy; the heap drop is the only
        // ownership work needed here.
        for value in self.stack.drain(..) {
            self.heap.drop_value(value);
        }
    }

    fn dispatch_inner<const DEBUG: bool>(
        &mut self,
        module: &Module,
        frames: &mut Vec<Frame>,
        mut observer: Option<&mut dyn VmDebugObserver>,
    ) -> Result<Value, VmError> {
        // A debug observer borrows the backtrace only for the duration of one
        // callback. Reuse one buffer across stops so stepping through a large
        // function does not allocate once per instruction. The non-debug
        // instantiation keeps the buffer empty, preserving the ordinary run's
        // allocation-free dispatch path.
        let mut debug_backtrace = if DEBUG {
            Vec::with_capacity(frames.len())
        } else {
            Vec::new()
        };
        loop {
            let depth = frames.len() - 1;
            let function_id = frames[depth].func;
            let pc = frames[depth].pc;
            let func = &module.functions[function_id as usize];
            // The bytecode is immutable for the duration of a run. Borrowing
            // the instruction avoids cloning path vectors and writeback target
            // tables on every dispatch iteration; scalar immediates are copied
            // only in the arms that need them.
            let instruction = &func.code[pc];
            if DEBUG && let Some(observer) = observer.as_deref_mut() {
                debug_backtrace.clear();
                debug_backtrace.extend(frames.iter().enumerate().rev().map(|(index, frame)| {
                    let function = &module.functions[frame.func as usize];
                    VmDebugFrame {
                        function_id: frame.func,
                        function_name: &function.name,
                        pc: if index == depth {
                            pc
                        } else {
                            frame.pc.saturating_sub(1)
                        },
                    }
                }));
                let event = VmDebugEvent {
                    function_id,
                    function_name: &func.name,
                    pc,
                    instruction,
                    code: &func.code,
                    call_depth: depth,
                    stack_depth: self.stack.len(),
                    locals: &frames[depth].locals,
                    stack: &self.stack,
                    backtrace: &debug_backtrace,
                };
                if matches!(observer.before_instruction(event), VmDebugAction::Stop) {
                    return Err(VmError::DebuggerStopped);
                }
            }
            frames[depth].pc = pc + 1;

            match instruction {
                Instruction::Return => {
                    let result = self.pop()?;
                    if let Some(value) = self.finish_frame(module, frames, result)? {
                        return Ok(value);
                    }
                }
                Instruction::ReturnVoid => {
                    let result = Value::Void;
                    if let Some(value) = self.finish_frame(module, frames, result)? {
                        return Ok(value);
                    }
                }
                Instruction::Call(index) => {
                    if frames.len() >= MAX_CALL_DEPTH {
                        return Err(VmError::CallDepthExceeded);
                    }
                    let mut callee = self.take_frame(module, *index)?;
                    if let Err(error) = self.fill_params(module, *index, &mut callee) {
                        // The callee is not on the frame stack yet, so the
                        // unwind cannot see it — the arguments already moved
                        // into its slots are freed here instead.
                        self.discard(callee.locals);
                        return Err(error);
                    }
                    frames.push(callee);
                }
                Instruction::CallNative(id) => self.call_native(module, *id, &[], frames)?,
                Instruction::CallForeign(id) => self.call_foreign(module, *id)?,
                Instruction::Jump(target) => {
                    // `func` is already the current function for this
                    // dispatch turn. Keep the validation guard, but avoid
                    // re-looking it up through `frame.func` on every loop
                    // back-edge.
                    if (*target as usize) >= func.code.len() {
                        return Err(VmError::BadJump(*target));
                    }
                    frames[depth].pc = *target as usize;
                }
                // Keep scalar loop shapes in this outer match. Falling
                // through to `step` would match the same instruction a second
                // time inside its large semantic dispatcher, which is visible
                // on every generated counter loop. The helpers retain the
                // checked ownership behavior used by the general path.
                Instruction::ConstInt(value) => self.stack.push(Value::Int(*value)),
                Instruction::ConstFloat(value) => self.stack.push(Value::Float(*value)),
                Instruction::ConstBool(value) => self.stack.push(Value::Bool(*value)),
                Instruction::ConstVoid => self.stack.push(Value::Void),
                Instruction::ConstRawPtrNull => self.stack.push(Value::RawPtr(0)),
                Instruction::LoadLocal(slot) => {
                    self.load_local(&frames[depth], *slot);
                }
                Instruction::StoreLocal(slot) => {
                    self.store_local(&mut frames[depth], *slot)?;
                }
                Instruction::JumpIfFalse(target) => {
                    if !self.pop_bool()? {
                        if (*target as usize) >= func.code.len() {
                            return Err(VmError::BadJump(*target));
                        }
                        frames[depth].pc = *target as usize;
                    }
                }
                Instruction::AddInt => self.add_int()?,
                Instruction::SubInt => self.sub_int()?,
                Instruction::MulInt => self.mul_int()?,
                Instruction::EqInt => self.eq_int()?,
                Instruction::NeInt => self.ne_int()?,
                Instruction::LtInt => self.lt_int()?,
                Instruction::LeInt => self.le_int()?,
                Instruction::GtInt => self.gt_int()?,
                Instruction::GeInt => self.ge_int()?,
                Instruction::CallMut { func, slot, path } => {
                    if frames.len() >= MAX_CALL_DEPTH {
                        return Err(VmError::CallDepthExceeded);
                    }
                    let mut callee = self.take_frame(module, *func)?;
                    // The writeback place's indices sit on top of the operand
                    // stack, pushed after the arguments; resolve them first so
                    // the arguments are exposed for `fill_params`.
                    let mut steps = Vec::new();
                    if let Err(error) = self.fill_steps(path, &mut steps) {
                        self.discard(callee.locals);
                        return Err(error);
                    }
                    if let Err(error) = self.fill_params(module, *func, &mut callee) {
                        self.discard(callee.locals);
                        return Err(error);
                    }
                    callee.writebacks.push(Writeback {
                        param: 0,
                        slot: *slot,
                        steps,
                    });
                    frames.push(callee);
                }
                Instruction::CallWriteback { func, targets } => {
                    if frames.len() >= MAX_CALL_DEPTH {
                        return Err(VmError::CallDepthExceeded);
                    }
                    let mut callee = self.take_frame(module, *func)?;
                    // Every target's place indices sit on top of the operand
                    // stack, pushed after the arguments and targets in order, so
                    // they are resolved back to front — the last target's
                    // indices are the ones on top.
                    let mut failure = None;
                    for target in targets.iter().rev() {
                        let mut steps = Vec::new();
                        if let Err(error) = self.fill_steps(&target.path, &mut steps) {
                            failure = Some(error);
                            break;
                        }
                        callee.writebacks.push(Writeback {
                            param: target.param,
                            slot: target.slot,
                            steps,
                        });
                    }
                    if let Some(error) = failure {
                        self.discard(callee.locals);
                        return Err(error);
                    }
                    // Back to parameter order, so a return writes the targets in
                    // the order the call declared them.
                    callee.writebacks.reverse();
                    if let Err(error) = self.fill_params(module, *func, &mut callee) {
                        self.discard(callee.locals);
                        return Err(error);
                    }
                    frames.push(callee);
                }
                Instruction::CallNativeWriteback { func, targets } => {
                    // The same stack protocol as `CallWriteback` — arguments,
                    // then each target's place indices, resolved back to front.
                    // What differs is where the final values come from: there is
                    // no callee frame to move them out of, so the call returns
                    // them and `call_native` stores them.
                    let mut writebacks = std::mem::take(&mut self.native_writebacks);
                    if writebacks.len() < targets.len() {
                        writebacks.resize_with(targets.len(), || Writeback {
                            param: 0,
                            slot: 0,
                            steps: Vec::new(),
                        });
                    }
                    let active = &mut writebacks[..targets.len()];
                    let mut failure = None;
                    for (writeback, target) in active.iter_mut().zip(targets.iter().rev()) {
                        writeback.param = target.param;
                        writeback.slot = target.slot;
                        if let Err(error) = self.fill_steps(&target.path, &mut writeback.steps) {
                            failure = Some(error);
                            break;
                        }
                    }
                    let result = if let Some(error) = failure {
                        Err(error)
                    } else {
                        // Back to parameter order for the returned writebacks.
                        active.reverse();
                        self.call_native(module, *func, active, frames)
                    };
                    for writeback in active {
                        writeback.steps.clear();
                    }
                    self.native_writebacks = writebacks;
                    result?;
                }
                other => self.step(module, &mut frames[depth], other)?,
            }
        }
    }

    #[inline(always)]
    fn jump(&self, module: &Module, frame: &mut Frame, target: u32) -> Result<(), VmError> {
        let len = module.functions[frame.func as usize].code.len() as u32;
        // A target must land on a real instruction; `len` (one past the end)
        // is out of range and would read past the code on the next step.
        if target >= len {
            return Err(VmError::BadJump(target));
        }
        frame.pc = target as usize;
        Ok(())
    }

    // ----- operand-stack helpers ---------------------------------------

    /// Pops a pointer word addressing C storage.
    fn pop_foreign_pointer(&mut self) -> Result<u64, VmError> {
        let value = self.pop()?;
        let Value::RawPtr(address) = value else {
            self.heap.drop_value(value);
            return Err(VmError::TypeMismatch {
                expected: "a pointer into C storage",
            });
        };
        Ok(address)
    }

    #[inline(always)]
    fn pop(&mut self) -> Result<Value, VmError> {
        self.stack.pop().ok_or(VmError::StackUnderflow)
    }

    /// Runs a callback with the reusable string-argument buffer removed from
    /// the VM, then returns the empty buffer to the VM for the next operation.
    fn with_string_args<R>(&mut self, body: impl FnOnce(&mut Self, &mut Vec<Value>) -> R) -> R {
        let mut arguments = std::mem::take(&mut self.string_args);
        let result = body(self, &mut arguments);
        arguments.clear();
        self.string_args = arguments;
        result
    }

    /// Reports a mismatched operand, freeing it first.
    ///
    /// The typed pops below take the value off the stack before they know it is
    /// the wrong one, and a popped value is this VM's to own. Well-typed
    /// bytecode never reaches here, but a `Module` is a public artifact and
    /// validation proves structure rather than stack typing — so an ill-typed
    /// module must trap without stranding storage in a heap that may outlive
    /// the call.
    fn mismatch(&mut self, value: Value, expected: &'static str) -> VmError {
        self.heap.drop_value(value);
        VmError::TypeMismatch { expected }
    }

    #[inline(always)]
    fn pop_int(&mut self) -> Result<i64, VmError> {
        match self.pop()? {
            Value::Int(value) => Ok(value),
            other => Err(self.mismatch(other, "Int")),
        }
    }

    #[inline(always)]
    fn pop_float(&mut self) -> Result<f64, VmError> {
        match self.pop()? {
            Value::Float(value) => Ok(value),
            other => Err(self.mismatch(other, "Float")),
        }
    }

    #[inline(always)]
    fn pop_bool(&mut self) -> Result<bool, VmError> {
        match self.pop()? {
            Value::Bool(value) => Ok(value),
            other => Err(self.mismatch(other, "Bool")),
        }
    }

    #[inline(always)]
    fn pop_str(&mut self) -> Result<crate::value::StrId, VmError> {
        match self.pop()? {
            Value::Str(id) => Ok(id),
            other => Err(self.mismatch(other, "String")),
        }
    }

    /// Pops the two string operands of a binary string op, right one first.
    ///
    /// Paired here rather than at each call site because the second pop is the
    /// one that can fail with the first already in hand: an ill-typed module
    /// would otherwise strand the right operand in a local no unwind can see.
    fn pop_two_str(&mut self) -> Result<(crate::value::StrId, crate::value::StrId), VmError> {
        let rhs = self.pop_str()?;
        match self.pop_str() {
            Ok(lhs) => Ok((lhs, rhs)),
            Err(error) => {
                self.heap.free(rhs);
                Err(error)
            }
        }
    }
}

/// The Kira value a seam scalar's bytes read back as.
///
/// The little-endian byte order is the seam's everywhere Kira builds; the eight
/// byte word is zero-padded above the scalar's own size by [`read_bytes`].
///
/// [`read_bytes`]: kira_runtime_abi::c_storage::read_bytes
fn foreign_scalar_value(ty: kira_runtime_abi::ForeignType, word: [u8; 8]) -> Value {
    use kira_runtime_abi::ForeignType;
    let raw = u64::from_le_bytes(word);
    match ty {
        // Signed types sign-extend from their own width; the zero padding above
        // the scalar is not part of the value.
        ForeignType::I8 => Value::Int(i64::from(raw as u8 as i8)),
        ForeignType::I16 => Value::Int(i64::from(raw as u16 as i16)),
        ForeignType::I32 => Value::Int(i64::from(raw as u32 as i32)),
        ForeignType::I64 => Value::Int(raw as i64),
        ForeignType::U8 => Value::Int(i64::from(raw as u8)),
        ForeignType::U16 => Value::Int(i64::from(raw as u16)),
        ForeignType::U32 => Value::Int(i64::from(raw as u32)),
        ForeignType::U64 => Value::Int(raw as i64),
        ForeignType::Bool => Value::Bool(raw != 0),
        ForeignType::F32 => Value::Float(f64::from(f32::from_bits(raw as u32))),
        ForeignType::F64 => Value::Float(f64::from_bits(raw)),
        ForeignType::RawPtr | ForeignType::CString => Value::RawPtr(raw),
        // Refused where the read is analyzed: a `Void` member has no bytes.
        ForeignType::Void => Value::Int(0),
    }
}

/// Writes one Kira value into a C buffer as `ty`.
///
/// The widths are the seam's, not Kira's: a `[F32]` holds `Value::Float`, which
/// is an `f64`, and what C reads is four bytes. Little-endian because that is
/// the byte order every target Kira builds for uses.
fn write_seam_scalar(
    out: &mut Vec<u8>,
    ty: kira_runtime_abi::ForeignType,
    value: Value,
) -> Result<(), VmError> {
    use kira_runtime_abi::ForeignType;
    let mismatch = VmError::TypeMismatch {
        expected: "an array element the C seam can carry",
    };
    match (ty, value) {
        (ForeignType::I8, Value::Int(n)) => out.push(n as u8),
        (ForeignType::U8 | ForeignType::Bool, Value::Int(n)) => out.push(n as u8),
        (ForeignType::Bool, Value::Bool(flag)) => out.push(u8::from(flag)),
        (ForeignType::I16 | ForeignType::U16, Value::Int(n)) => {
            out.extend_from_slice(&(n as u16).to_le_bytes());
        }
        (ForeignType::I32 | ForeignType::U32, Value::Int(n)) => {
            out.extend_from_slice(&(n as u32).to_le_bytes());
        }
        (ForeignType::I64 | ForeignType::U64, Value::Int(n)) => {
            out.extend_from_slice(&n.to_le_bytes());
        }
        (ForeignType::F32, Value::Float(x)) => out.extend_from_slice(&(x as f32).to_le_bytes()),
        (ForeignType::F64, Value::Float(x)) => out.extend_from_slice(&x.to_le_bytes()),
        (ForeignType::RawPtr, Value::RawPtr(word)) => {
            out.extend_from_slice(&word.to_le_bytes());
        }
        _ => return Err(mismatch),
    }
    Ok(())
}
