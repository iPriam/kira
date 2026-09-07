//! Execution of bytecode instructions that cross the host main-thread boundary.

use kira_runtime_abi::{MainThreadOp, MainThreadRequest, MainThreadResponse};

use super::Vm;
use crate::error::VmError;
use crate::value::Value;

impl Vm<'_> {
    /// Copies values off the operand stack and asks the host main-thread loop
    /// to service one resolved function call.
    pub(super) fn main_thread_call(
        &mut self,
        operation: MainThreadOp,
        function: u64,
        count: u64,
    ) -> Result<(), VmError> {
        let function_id =
            u32::try_from(function).map_err(|_| VmError::UnknownFunction(function))?;
        let count = usize::try_from(count).map_err(|_| VmError::StackUnderflow)?;
        let mut values = Vec::with_capacity(count);
        for _ in 0..count {
            match self.pop() {
                Ok(value) => values.push(value),
                Err(error) => {
                    self.discard(values);
                    return Err(error);
                }
            }
        }
        values.reverse();

        let mut request_args = Vec::with_capacity(values.len());
        for &value in &values {
            let Some(tree) = self.heap.seam_tree(value) else {
                self.discard(values);
                return Err(VmError::MainThreadValue { function });
            };
            request_args.push(tree);
        }
        self.discard(values);
        let response = self
            .host
            .main_thread(MainThreadRequest::new(operation, function_id, request_args))
            .map_err(VmError::MainThread)?;
        match (operation, response) {
            (MainThreadOp::Invoke, MainThreadResponse::Value(value)) => {
                self.stack.push(self.heap.from_native_state(&value));
            }
            // A void invocation has no state-tree representation. Hosts use
            // `Posted` for that case, which is also the acknowledgement for a
            // fire-and-forget post.
            (
                MainThreadOp::Invoke | MainThreadOp::Post | MainThreadOp::LifecycleStart,
                MainThreadResponse::Posted,
            ) => {
                self.stack.push(Value::Void);
            }
            (MainThreadOp::Spawn, MainThreadResponse::Spawned(handle)) => {
                self.stack.push(Value::MainThreadTask(handle));
            }
            (operation, _) => {
                return Err(VmError::MainThreadResponse {
                    operation: operation.label(),
                });
            }
        }
        Ok(())
    }

    /// Joins a task owned by the host main-thread loop.
    pub(super) fn main_thread_join(&mut self) -> Result<(), VmError> {
        let value = self.pop()?;
        let Value::MainThreadTask(handle) = value else {
            self.heap.drop_value(value);
            return Err(VmError::MainThreadHandleMismatch);
        };
        let result = self
            .host
            .main_thread_join(handle)
            .map_err(VmError::MainThread)?;
        self.stack.push(self.heap.from_native_state(&result));
        Ok(())
    }
}
