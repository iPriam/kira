//! Host-capability calls made by bytecode instructions.

use kira_bytecode::module::Module;
use kira_runtime_abi::{ForeignArg, ForeignPointerWidth, ForeignResult, NativeArg};

use super::Vm;
use crate::error::VmError;
use crate::value::Value;

impl Vm<'_> {
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
                Value::NativeState(_) | Value::NativeView { .. } => {
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

        // An aggregate argument's C-layout bytes are built here and held for the
        // whole call: `ForeignArg::Aggregate` borrows them, so the buffers must
        // outlive `lowered` and cannot live inside the loop.
        let width = ForeignPointerWidth::HOST;
        let mut aggregate_bytes: Vec<(usize, Vec<u8>)> = Vec::new();
        let mut mismatch = None;
        for (offset, &expected) in params.iter().enumerate() {
            let Some(id) = expected.aggregate() else {
                continue;
            };
            let value = self.stack[first + offset];
            match self
                .heap
                .aggregate_bytes(&module.foreign_aggregates, id, value, width)
            {
                Some(bytes) => aggregate_bytes.push((offset, bytes)),
                None => {
                    mismatch = Some(expected);
                    break;
                }
            }
        }

        let mut lowered = Vec::with_capacity(count);
        if mismatch.is_none() {
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
        let outcome = match mismatch {
            Some(expected) => Err(VmError::ForeignArgMismatch {
                foreign: id,
                expected,
            }),
            None => self
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
