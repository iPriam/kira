//! Environment instruction execution.
//!
//! Unlike the file system and the compiler, this asks no host: the process
//! environment and arguments are available wherever the VM runs, and are
//! read-only. The answer comes from [`kira_runtime_abi::env`], which native
//! code reaches through `kira_rt_env_*` — one definition, so a program cannot
//! get one answer on one backend and another on the next.

use kira_runtime_abi::{EnvOp, env};

use super::Vm;
use crate::error::VmError;
use crate::value::Value;

impl Vm<'_> {
    /// Runs one environment operation, pushing its result.
    ///
    /// The operand is dropped on every path out, including the error one, so a
    /// refused read leaks no heap.
    /// Pops one operand and reads it as the name an environment op asks about,
    /// dropping it either way.
    fn pop_env_name(&mut self) -> Result<String, VmError> {
        let operand = self.pop()?;
        let name = match &operand {
            Value::Str(id) => Ok(self.heap.get(*id).to_owned()),
            _ => Err(VmError::NotAString),
        };
        self.heap.drop_value(operand);
        name
    }

    pub(super) fn env(&mut self, op: EnvOp) -> Result<(), VmError> {
        // Every arm is total by construction: the outer match discriminates on
        // the same op each arm answers, so no arm needs to promise that
        // impossible cases never arrive.
        let value = match op {
            EnvOp::Text => {
                let name = self.pop_env_name()?;
                Value::Str(self.heap.alloc(env::text(&name)))
            }
            EnvOp::IsSet => {
                let name = self.pop_env_name()?;
                Value::Bool(env::is_set(&name))
            }
            EnvOp::ArgumentCount => Value::Int(env::argument_count()),
            EnvOp::Argument => {
                let index = self.pop_int()?;
                Value::Str(self.heap.alloc(env::argument(index)))
            }
            EnvOp::Sleep => {
                let milliseconds = self.pop_int()?;
                env::sleep(milliseconds);
                Value::Void
            }
        };
        self.stack.push(value);
        Ok(())
    }
}
