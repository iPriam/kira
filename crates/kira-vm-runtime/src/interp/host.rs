//! Host-capability calls made by bytecode instructions.

use kira_bytecode::module::Module;
use kira_runtime_abi::{ForeignArg, ForeignPointerWidth, ForeignResult, NativeArg};

use super::Vm;
use crate::error::VmError;
use crate::value::{AggregateMismatch, Value};

impl Vm<'_> {
    /// Rebuilds every deferred state read sitting at or above `first` on the
    /// operand stack.
    ///
    /// A seam call is where a value stops being the VM's: the other side reads
    /// it as an object, so the deferral cannot cross.
    pub(super) fn own_arguments(&mut self, first: usize) {
        for index in first..self.stack.len() {
            if matches!(self.stack[index], Value::NativeSnapshot(_)) {
                self.stack[index] = self.heap.own(self.stack[index]);
            }
        }
    }

    /// Calls into the native half through the embedder.
    pub(super) fn call_native(&mut self, module: &Module, id: u32) -> Result<(), VmError> {
        let proto = module
            .functions
            .get(id as usize)
            .ok_or(VmError::UnknownFunction(id))?;
        let count = proto.param_count as usize;
        let first = self
            .stack
            .len()
            .checked_sub(count)
            .ok_or(VmError::StackUnderflow)?;
        // A deferred state read becomes objects before it reaches the seam, so
        // an aggregate arriving here is refused by the shape it actually has
        // rather than as an opaque handle.
        self.own_arguments(first);
        let arguments = &self.stack[first..];

        let mut lowered = Vec::with_capacity(count);
        for value in arguments {
            lowered.push(match *value {
                Value::Int(value) => NativeArg::Int(value),
                Value::Float(value) => NativeArg::Float(value),
                Value::Bool(value) => NativeArg::Bool(value),
                Value::Str(id) => NativeArg::Str(self.heap.get(id)),
                Value::Void => NativeArg::Void,
                Value::Struct(_) => return Err(VmError::StructAtSeam { function: id }),
                Value::Array(_) => return Err(VmError::ArrayAtSeam { function: id }),
                Value::Enum(_) => return Err(VmError::EnumAtSeam { function: id }),
                Value::RawPtr(value) => NativeArg::RawPtr(value),
                // A cell is refused with the handles, and for the strongest
                // reason among them: it is shared mutable storage this heap
                // counts holds on, and the other side of the seam has no way to
                // release one. The compiler never routes one here — a cell is
                // not surface and cannot appear in a signature — so this is a
                // guard on the desugar, not a message a reader can provoke.
                // An erased value is refused with them. `Any` is not a foreign
                // signature type — nothing in a C signature spells the top
                // type — so, like a cell, this guards the desugar rather than
                // reporting something a reader can provoke.
                // A deferred read is refused with them, and is unreachable:
                // `own_arguments` above rebuilt every one on this stack, so a
                // state read arrives as the struct, array or enum it holds.
                Value::NativeState(_)
                | Value::NativeView { .. }
                | Value::NativeSnapshot(_)
                | Value::Cell(_)
                | Value::Erased(_) => {
                    return Err(VmError::HandleAtSeam { function: id });
                }
            });
        }
        let returned = self
            .host
            .call_native(id, &lowered)
            .map_err(VmError::NativeCall);

        for value in self.stack.split_off(first) {
            self.heap.drop_value(value);
        }

        let result = self
            .heap
            .absorb(returned?)
            .ok_or(VmError::HandleAtSeam { function: id })?;
        self.stack.push(result);
        Ok(())
    }

    /// Calls a foreign C function through the embedder's `call_foreign`.
    /// Pushes the address C enters the Kira function callback `id` names.
    ///
    /// The VM produces no native code, so the address is the host's to give: it
    /// is the entry thunk the backend generated for this row. A host without a
    /// foreign half refuses, exactly as it refuses a foreign call.
    pub(super) fn foreign_callback(&mut self, module: &Module, id: u32) -> Result<(), VmError> {
        if module.foreign_callbacks.get(id as usize).is_none() {
            return Err(VmError::UnknownForeignCallback(id));
        }
        let address = self
            .host
            .foreign_callback(id)
            .map_err(VmError::ForeignCall)?;
        self.stack.push(Value::RawPtr(address));
        Ok(())
    }

    pub(super) fn call_foreign(&mut self, module: &Module, id: u32) -> Result<(), VmError> {
        let import = module
            .foreign_imports
            .get(id as usize)
            .ok_or(VmError::UnknownForeign(id))?;
        let params = import.signature().parameters();
        let count = params.len();
        let first = self
            .stack
            .len()
            .checked_sub(count)
            .ok_or(VmError::StackUnderflow)?;
        // Same reason as the native seam: an aggregate crossing into C is laid
        // out from real objects, so the deferral ends before the lowering does.
        self.own_arguments(first);

        // An aggregate argument's C-layout bytes are built here and held for the
        // whole call: `ForeignArg::Aggregate` borrows them, so the buffers must
        // outlive `lowered` and cannot live inside the loop.
        let width = ForeignPointerWidth::HOST;
        let mut aggregate_bytes: Vec<(usize, Vec<u8>)> = Vec::new();
        let mut mismatch = None;
        let mut too_long: Option<(u32, usize)> = None;
        for (offset, &expected) in params.iter().enumerate() {
            let Some(id) = expected.aggregate() else {
                continue;
            };
            let value = self.stack[first + offset];
            match self
                .heap
                .aggregate_bytes(&module.foreign_aggregates, id, value, width)
            {
                Ok(bytes) => aggregate_bytes.push((offset, bytes)),
                // An array that does not fit is the program's own mistake, so it
                // is reported as itself rather than folded into the shape
                // mismatch that means the compiler built the table wrong.
                Err(AggregateMismatch::ArrayTooLong { count, len }) => {
                    too_long = Some((count, len));
                    break;
                }
                Err(AggregateMismatch::Shape) => {
                    mismatch = Some(expected);
                    break;
                }
            }
        }

        let mut lowered = Vec::with_capacity(count);
        if mismatch.is_none() && too_long.is_none() {
            for (offset, &expected) in params.iter().enumerate() {
                let value = self.stack[first + offset];
                let argument =
                    match expected.aggregate() {
                        Some(id) => aggregate_bytes.iter().find(|(at, _)| *at == offset).map(
                            |(_, bytes)| ForeignArg::Aggregate {
                                id,
                                bytes: bytes.as_slice(),
                            },
                        ),
                        None => self.heap.foreign_arg(expected, value),
                    };
                match argument {
                    Some(argument) => lowered.push(argument),
                    None => {
                        mismatch = Some(expected);
                        break;
                    }
                }
            }
        }
        let outcome = match (too_long, mismatch) {
            (Some((count, len)), _) => Err(VmError::ForeignArrayTooLong { count, len }),
            (None, Some(expected)) => Err(VmError::ForeignArgMismatch {
                foreign: id,
                expected,
            }),
            (None, None) => self
                .host
                .call_foreign(id, &lowered)
                .map_err(VmError::ForeignCall),
        };
        drop(lowered);
        for value in self.stack.split_off(first) {
            self.heap.drop_value(value);
        }

        let outcome = outcome?;
        let spec = outcome.spec();
        // An aggregate result is rebuilt into a Kira struct here rather than in
        // `absorb_foreign`: the member tree lives on the module, which the heap
        // has no access to.
        let result = match outcome {
            ForeignResult::Aggregate {
                id: aggregate,
                ref bytes,
            } => self
                .heap
                .absorb_aggregate(&module.foreign_aggregates, aggregate, bytes, width),
            other => self.heap.absorb_foreign(other),
        };
        let result = result.ok_or(VmError::ForeignArgMismatch {
            foreign: id,
            expected: spec,
        })?;
        self.stack.push(result);
        Ok(())
    }
}
