//! Opaque native callback-state instruction execution.

use super::Vm;
use crate::error::VmError;
use crate::value::Value;

impl Vm<'_> {
    pub(super) fn native_state_new(&mut self, type_word: u64) -> Result<(), VmError> {
        let value = self.pop()?;
        let stored = self
            .heap
            .into_native_state(value)
            .ok_or(VmError::NativeStateValueMismatch)?;
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
            return Err(VmError::NativeStateValueMismatch);
        };
        self.stack.push(Value::RawPtr(token.as_word()));
        Ok(())
    }

    pub(super) fn native_recover(&mut self, type_word: u64) -> Result<(), VmError> {
        let raw = self.pop()?;
        let Value::RawPtr(word) = raw else {
            self.heap.drop_value(raw);
            return Err(VmError::NativeStateValueMismatch);
        };
        let token = kira_runtime_abi::NativeStateToken::from_word(word);
        let type_id = kira_runtime_abi::NativeStateTypeId::new(type_word);
        self.host
            .native_state_recover(token, type_id)
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
                return Err(VmError::NativeStateValueMismatch);
            }
        };
        self.host
            .native_state_free(token)
            .map_err(VmError::NativeState)?;
        self.stack.push(Value::Void);
        Ok(())
    }
}
