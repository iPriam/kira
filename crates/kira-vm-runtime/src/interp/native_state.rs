//! Opaque native callback-state instruction execution.

use super::Vm;
use crate::error::{NativeStateOperation, VmError};
use crate::value::Value;

impl Vm<'_> {
    pub(super) fn native_state_new(&mut self, type_word: u64) -> Result<(), VmError> {
        let value = self.pop()?;
        let stored = self.heap.into_native_state(value).map_err(|kind| {
            VmError::NativeStateValueMismatch {
                operation: NativeStateOperation::Store,
                kind,
            }
        })?;
        let type_id = kira_runtime_abi::NativeStateTypeId::new(type_word);
        let token = self
            .host
            .native_state_create(type_id, stored)
            .map_err(VmError::NativeState)?;
        self.stack.push(Value::NativeState(token));
        Ok(())
    }

    pub(super) fn native_user_data(&mut self) -> Result<(), VmError> {
        let state = self.pop()?;
        let Value::NativeState(token) = state else {
            self.heap.drop_value(state);
            return Err(VmError::NativeStateValueMismatch {
                operation: NativeStateOperation::UserData,
                kind: "a value that is not callback state",
            });
        };
        self.stack.push(Value::RawPtr(token.as_word()));
        Ok(())
    }

    pub(super) fn native_recover(&mut self, type_word: u64) -> Result<(), VmError> {
        let raw = self.pop()?;
        let Value::RawPtr(word) = raw else {
            self.heap.drop_value(raw);
            return Err(VmError::NativeStateValueMismatch {
                operation: NativeStateOperation::Recover,
                kind: "a value that is not a callback-state token",
            });
        };
        let token = kira_runtime_abi::NativeStateToken::from_word(word);
        let type_id = kira_runtime_abi::NativeStateTypeId::new(type_word);
        // Check the token and type, and read nothing: what goes on the stack is
        // a handle. This used to recover the state — a deep copy of everything
        // it holds, discarded immediately — on every recovery, which is once per
        // function that touches it and many times per frame.
        self.host
            .native_state_check(token, type_id)
            .map_err(VmError::NativeState)?;
        self.stack.push(Value::NativeView { token, type_id });
        Ok(())
    }

    pub(super) fn native_state_free(&mut self) -> Result<(), VmError> {
        let value = self.pop()?;
        let token = match value {
            Value::NativeState(token) => token,
            Value::RawPtr(word) => kira_runtime_abi::NativeStateToken::from_word(word),
            other => {
                self.heap.drop_value(other);
                return Err(VmError::NativeStateValueMismatch {
                    operation: NativeStateOperation::Free,
                    kind: "a value that is neither callback state nor a token",
                });
            }
        };
        self.host
            .native_state_free(token)
            .map_err(VmError::NativeState)?;
        self.stack.push(Value::Void);
        Ok(())
    }
}
