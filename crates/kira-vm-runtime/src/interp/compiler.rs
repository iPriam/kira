//! Compiler instruction execution.
//!
//! The VM compiles nothing itself — it sits below the compiler in the layering
//! and could never hold one. It pops the request array, reads it as a
//! [`CheckRequest`], and hands that to the host, exactly as it hands a
//! [`FileRequest`](kira_runtime_abi::FileRequest) to the same host. A host that
//! links the frontend answers; one that does not says so by name.

use kira_runtime_abi::{CheckDiagnostic, CheckRequest, CompilerOp};

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
        let mut operands = Vec::with_capacity(op.arity());
        for _ in 0..op.arity() {
            operands.push(self.pop()?);
        }
        operands.reverse();
        let result = self.compiler_call(op, &operands);
        for operand in operands {
            self.heap.drop_value(operand);
        }
        let diagnostics = result?;
        let value = self.diagnostics_value(&diagnostics);
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
    ) -> Result<Vec<CheckDiagnostic>, VmError> {
        match op {
            CompilerOp::CheckPackages => {
                let fields = self.string_array(operands.first())?;
                let request = CheckRequest::decode(&fields)
                    .map_err(|error| VmError::Compiler(error.into()))?;
                self.host.compiler(&request).map_err(VmError::Compiler)
            }
        }
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

    /// Turns the host's diagnostics into the `[String]` the instruction pushes.
    fn diagnostics_value(&mut self, diagnostics: &[CheckDiagnostic]) -> Value {
        let elements: Vec<Value> = CheckDiagnostic::encode(diagnostics)
            .into_iter()
            .map(|field| Value::Str(self.heap.alloc(field)))
            .collect();
        Value::Array(self.heap.alloc_array(elements))
    }
}
