//! File-system instruction execution.
//!
//! The VM performs none of this itself: it pops the operands, describes what the
//! program asked for, and hands the description to the host. That is what keeps
//! the interpreter free of a filesystem dependency and buildable for
//! `wasm32-unknown-unknown`, and it is the same shape `print` already has.

use kira_runtime_abi::{FileRequest, FileResponse, FileSystemOp};

use super::Vm;
use crate::error::VmError;
use crate::value::Value;

impl Vm<'_> {
    /// Runs one file-system operation, pushing its result.
    ///
    /// The operands come off the stack innermost-last, so they are popped in
    /// reverse and reversed back into source order before anything reads them.
    /// Every popped value is dropped on every path out, including the error
    /// paths, so a refused host call leaks no heap.
    pub(super) fn file_system(&mut self, op: FileSystemOp) -> Result<(), VmError> {
        let mut operands = self.pop_operands(op.arity())?;
        operands.reverse();
        let result = self.file_system_call(op, &operands);
        for operand in operands {
            self.heap.drop_value(operand);
        }
        let response = result?;
        let value = self.file_system_value(response);
        self.stack.push(value);
        Ok(())
    }

    /// Builds the request from already-popped operands and performs it.
    ///
    /// Split out so the caller owns the drops: this borrows the operands rather
    /// than consuming them, which is what lets an error still free them.
    fn file_system_call(
        &mut self,
        op: FileSystemOp,
        operands: &[Value],
    ) -> Result<FileResponse, VmError> {
        // The strings are read out of the heap first and held as owned copies:
        // the host call borrows `self` mutably, so a `&str` into the heap could
        // not survive it.
        let mut texts = Vec::with_capacity(operands.len());
        for operand in operands {
            texts.push(match operand {
                Value::Str(id) => Some(self.heap.get(*id).to_owned()),
                _ => None,
            });
        }
        let text = |index: usize| -> Result<&str, VmError> {
            texts
                .get(index)
                .and_then(Option::as_deref)
                .ok_or(VmError::NotAString)
        };

        let request = match op {
            FileSystemOp::ReadRange => {
                let (Some(Value::Int(offset)), Some(Value::Int(count))) =
                    (operands.get(1), operands.get(2))
                else {
                    return Err(VmError::TypeMismatch { expected: "Int" });
                };
                FileRequest::ReadRange {
                    path: text(0)?,
                    offset: *offset,
                    count: *count,
                }
            }
            FileSystemOp::WriteBytes => {
                let Some(Value::Array(id)) = operands.get(1) else {
                    return Err(VmError::NotAnArray);
                };
                let mut bytes = Vec::with_capacity(self.heap.array_len(*id).unwrap_or(0));
                for element in self.heap.elements(*id) {
                    let Value::Int(byte) = element else {
                        return Err(VmError::TypeMismatch { expected: "Int" });
                    };
                    bytes.push(*byte as u8);
                }
                // The borrow of the heap ends with `bytes`, so the request can
                // hold a slice of it across the host call.
                return self.perform(FileRequest::WriteBytes {
                    path: texts
                        .first()
                        .and_then(Option::as_deref)
                        .ok_or(VmError::NotAString)?,
                    bytes: &bytes,
                });
            }
            FileSystemOp::ReadText => FileRequest::ReadText { path: text(0)? },
            FileSystemOp::WriteText => FileRequest::WriteText {
                path: text(0)?,
                text: text(1)?,
            },
            FileSystemOp::ListDirectory => FileRequest::ListDirectory { path: text(0)? },
            FileSystemOp::IsDirectory => FileRequest::IsDirectory { path: text(0)? },
            FileSystemOp::MakeDirectory => FileRequest::MakeDirectory { path: text(0)? },
            FileSystemOp::RenamePath => FileRequest::RenamePath {
                from: text(0)?,
                to: text(1)?,
            },
            FileSystemOp::RemovePath => FileRequest::RemovePath { path: text(0)? },
            FileSystemOp::FileExists => FileRequest::FileExists { path: text(0)? },
            FileSystemOp::PathExists => FileRequest::PathExists { path: text(0)? },
            FileSystemOp::FileSize => FileRequest::FileSize { path: text(0)? },
        };
        self.perform(request)
    }

    /// Hands one request to the host, mapping its refusal to a VM error.
    fn perform(&mut self, request: FileRequest<'_>) -> Result<FileResponse, VmError> {
        self.host.file_system(request).map_err(VmError::FileSystem)
    }

    /// Turns a host response into the runtime value the instruction pushes.
    fn file_system_value(&mut self, response: FileResponse) -> Value {
        match response {
            FileResponse::Bytes(bytes) => {
                let elements = bytes.into_iter().map(|byte| Value::Int(i64::from(byte)));
                Value::Array(self.heap.alloc_array(elements.collect()))
            }
            FileResponse::Text(text) => Value::Str(self.heap.alloc(text)),
            FileResponse::Names(names) => {
                let elements: Vec<Value> = names
                    .into_iter()
                    .map(|name| Value::Str(self.heap.alloc(name)))
                    .collect();
                Value::Array(self.heap.alloc_array(elements))
            }
            FileResponse::Flag(flag) => Value::Bool(flag),
            // A `U64` is one of the eight integer spellings over the same 64-bit
            // representation, so a size crosses as the same bits the native
            // engine returns rather than being narrowed here.
            FileResponse::Size(size) => Value::Int(size as i64),
        }
    }
}
