//! Environment instruction execution.
//!
//! Unlike the file system and the compiler, this asks no host: the process
//! environment is the one thing the VM can read wherever it runs, and it is
//! read-only, so there is nothing for an embedder to police. The answer comes
//! from [`kira_runtime_abi::env`], which native code reaches through
//! `kira_rt_env_*` — one definition, so a program cannot get one answer on one
//! backend and another on the next.

use kira_runtime_abi::{EnvOp, env};

use super::Vm;
use crate::error::VmError;
use crate::value::Value;

impl Vm<'_> {
    /// Runs one environment operation, pushing its result.
    ///
    /// The operand is dropped on every path out, including the error one, so a
    /// refused read leaks no heap.
    pub(super) fn env(&mut self, op: EnvOp) -> Result<(), VmError> {
        let operand = self.pop()?;
        let name = match &operand {
            Value::Str(id) => Ok(self.heap.get(*id).to_owned()),
            _ => Err(VmError::NotAString),
        };
        self.heap.drop_value(operand);
        let name = name?;
        let value = match op {
            EnvOp::Text => Value::Str(self.heap.alloc(env::text(&name))),
            EnvOp::IsSet => Value::Bool(env::is_set(&name)),
        };
        self.stack.push(value);
        Ok(())
    }
}
