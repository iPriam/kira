//! Compiler instruction execution.
//!
//! The VM compiles nothing itself — it sits below the compiler in the layering
//! and could never hold one. It pops the request array, reads it as a
//! [`CheckRequest`] or a [`ToolRequest`], and hands that to the host, exactly
//! as it hands a [`FileRequest`](kira_runtime_abi::FileRequest) to the same
//! host. A host that links the frontend answers; one that does not says so by
//! name.
//!
//! Which of the two request layouts an operation carries follows from the
//! operation alone: [`CompilerOp::verb`] names one for the three that work on a
//! package on disk, and none for the in-memory check. Both directions are
//! `[String]`, so everything after the decode is the same code.

use kira_runtime_abi::{
    CheckDiagnostic, CheckRequest, CompilerOp, ToolAnswer, ToolRequest,
};

use super::Vm;
use crate::error::VmError;
use crate::value::Value;

impl Vm<'_> {
    /// Runs one compiler operation, pushing its result.
    ///
    /// The operands come off the stack innermost-last, so they are popped in
    /// reverse and reversed back into source order. Every popped value is
    /// dropped on every path out, including the error paths, so a refused host
    /// call leaks no heap.
    pub(super) fn compiler(&mut self, op: CompilerOp) -> Result<(), VmError> {
        let mut operands = self.pop_operands(op.arity())?;
        operands.reverse();
        let result = self.compiler_call(op, &operands);
        for operand in operands {
            self.heap.drop_value(operand);
        }
        let fields = result?;
        let value = self.string_array_value(&fields);
        self.stack.push(value);
        Ok(())
    }

    /// Builds the request from already-popped operands and performs it.
    ///
    /// Split out so the caller owns the drops: this borrows the operands rather
    /// than consuming them, which is what lets an error still free them.
    fn compiler_call(
        &mut self,
        op: CompilerOp,
        operands: &[Value],
    ) -> Result<Vec<String>, VmError> {
        let fields = self.string_array(operands.first())?;
        let Some(verb) = op.verb() else {
            let request =
                CheckRequest::decode(&fields).map_err(|error| VmError::Compiler(error.into()))?;
            let diagnostics = self.host.compiler(&request).map_err(VmError::Compiler)?;
            return Ok(CheckDiagnostic::encode(&diagnostics));
        };
        let request =
            ToolRequest::decode(&fields).map_err(|error| VmError::Toolchain(error.into()))?;
        let answer: ToolAnswer = self
            .host
            .toolchain(verb, &request)
            .map_err(VmError::Toolchain)?;
        Ok(answer.encode())
    }

    /// Reads a `[String]` operand out of the heap as owned text.
    ///
    /// Owned rather than borrowed because the host call takes `self` mutably: a
    /// `&str` into the heap could not survive it.
    fn string_array(&self, operand: Option<&Value>) -> Result<Vec<String>, VmError> {
        let Some(Value::Array(id)) = operand else {
            return Err(VmError::NotAnArray);
        };
        self.heap
            .elements(*id)
            .iter()
            .map(|element| match element {
                Value::Str(text) => Ok(self.heap.get(*text).to_owned()),
                _ => Err(VmError::NotAString),
            })
            .collect()
    }

    /// Turns the host's answer into the `[String]` the instruction pushes.
    fn string_array_value(&mut self, fields: &[String]) -> Value {
        let elements: Vec<Value> = fields
            .iter()
            .map(|field| Value::Str(self.heap.alloc(field.clone())))
            .collect();
        Value::Array(self.heap.alloc_array(elements))
    }
}
