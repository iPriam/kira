//! Opaque native callback-state instruction execution.
//!
//! # Reading state costs the read, not the state
//!
//! Two deferrals, one after the other. `nativeRecover` hands back a *view* — a
//! checked token, nothing read — and reading a field through that view hands
//! back a *snapshot*, the stored node itself rather than a subtree rebuilt as
//! heap objects. A walk over a UI tree held in callback state therefore
//! allocates nothing until it reaches a leaf worth computing with, and the
//! aggregates it passes through cost a refcount each.
//!
//! Neither deferral is visible to a program. The stored node shares its children
//! and a write to the state unshares them
//! ([`kira_runtime_abi::NativeStateValue`]), so a snapshot keeps showing what was
//! read; and anything that would *edit* a snapshot rebuilds it as objects first
//! ([`crate::value::Heap::own`]), so nothing is ever written through one.

use kira_runtime_abi::{NativeStatePathStep, NativeStateValue};

use super::Vm;
use crate::error::{NativeStateOperation, VmError};
use crate::value::{SnapshotId, Value};

impl Vm<'_> {
    /// Reads one child out of a deferred state read, consuming the read.
    ///
    /// `step` says which child *and* which shape it must come from: a field
    /// step reads a struct and an index step reads an array, so a path can
    /// never read a struct's third field as an array element. The shapes that
    /// disagree get `mismatch`, which is the trap the equivalent read of a real
    /// object would have raised.
    pub(super) fn read_snapshot_child(
        &mut self,
        id: SnapshotId,
        step: NativeStatePathStep,
        mismatch: VmError,
    ) -> Result<Value, VmError> {
        let child = match (self.heap.snapshot_node(id), step) {
            (Some(NativeStateValue::Struct(fields)), NativeStatePathStep::Field(index)) => {
                fields.get(index as usize).cloned()
            }
            (Some(NativeStateValue::Array(elements)), NativeStatePathStep::Index(index)) => {
                usize::try_from(index)
                    .ok()
                    .and_then(|index| elements.get(index))
                    .cloned()
            }
            _ => {
                self.heap.free_snapshot(id);
                return Err(mismatch);
            }
        };
        self.heap.free_snapshot(id);
        // The child was cloned before the read was freed, which for an
        // aggregate is a refcount bump rather than a copy.
        let Some(child) = child else {
            return Err(mismatch);
        };
        Ok(self.heap.read_state_node(child))
    }

    /// The length of the array a deferred read landed on, consuming the read.
    pub(super) fn snapshot_array_len(&mut self, id: SnapshotId) -> Result<usize, VmError> {
        let len = match self.heap.snapshot_node(id) {
            Some(NativeStateValue::Array(elements)) => Some(elements.len()),
            _ => None,
        };
        self.heap.free_snapshot(id);
        len.ok_or(VmError::NotAnArray)
    }

    /// The tag of the enum a deferred read landed on, consuming the read.
    pub(super) fn snapshot_enum_tag(&mut self, id: SnapshotId) -> Result<u64, VmError> {
        let tag = match self.heap.snapshot_node(id) {
            Some(NativeStateValue::Enum { tag, .. }) => Some(u64::from(*tag)),
            _ => None,
        };
        self.heap.free_snapshot(id);
        tag.ok_or(VmError::NotAnEnum)
    }

    /// The payload of the enum a deferred read landed on, consuming the read.
    pub(super) fn snapshot_enum_payload(&mut self, id: SnapshotId) -> Result<Value, VmError> {
        let payload = match self.heap.snapshot_node(id) {
            Some(NativeStateValue::Enum { payload, .. }) => Some(
                payload
                    .as_deref()
                    .cloned()
                    .ok_or(VmError::MissingEnumPayload),
            ),
            _ => None,
        };
        self.heap.free_snapshot(id);
        let payload = payload.ok_or(VmError::NotAnEnum)??;
        Ok(self.heap.read_state_node(payload))
    }

    pub(super) fn native_state_new(&mut self, type_word: u64) -> Result<(), VmError> {
        let value = self.pop()?;
        let type_id = kira_runtime_abi::NativeStateTypeId::new(type_word);
        let stored = self.heap.into_native_state(value).map_err(|kind| {
            VmError::NativeStateValueMismatch {
                operation: NativeStateOperation::Store,
                kind,
            }
        })?;
        let token = self
            .host
            .native_state_create(type_id, stored)
            .map_err(VmError::NativeState)?;
        self.stack.push(Value::NativeState(token));
        Ok(())
    }

    pub(super) fn native_user_data(&mut self, shared: bool) -> Result<(), VmError> {
        let state = self.pop()?;
        let Value::NativeState(token) = state else {
            self.heap.drop_value(state);
            return Err(VmError::NativeStateValueMismatch {
                operation: NativeStateOperation::UserData,
                kind: "a value that is not callback state",
            });
        };
        // The token owns one reference, and the value just popped is it: a
        // handle reaches this stack either as a temporary, which owns the
        // reference it was created with, or as a load, which the heap copied
        // and counted on the way out of its slot. Either way exactly one
        // reference arrives here and the token takes it over.
        //
        // `shared` therefore changes nothing on this engine, and is not
        // ignored so much as already answered. It is what native code reads:
        // there a load is a raw word that took no reference, so the shared
        // case has to take one. The instruction says where the handle came
        // from, and each engine answers in its own terms.
        let _ = shared;
        self.stack.push(Value::RawPtr(token.as_word()));
        Ok(())
    }

    pub(super) fn native_state_retain(&mut self) -> Result<(), VmError> {
        let token = self.pop_state_token(NativeStateOperation::Retain)?;
        self.host
            .native_state_retain(token)
            .map_err(VmError::NativeState)?;
        self.stack.push(Value::Void);
        Ok(())
    }

    /// Settles the reference changes the heap recorded while copying and
    /// dropping handles, retains before releases so a copy-and-drop of one
    /// handle never destroys the state between the two.
    pub(super) fn settle_native_state(&mut self) -> Result<(), VmError> {
        let (retains, releases) = self.heap.take_native_state_events();
        for token in retains {
            self.host
                .native_state_retain(token)
                .map_err(VmError::NativeState)?;
        }
        for token in releases {
            self.host
                .native_state_release(token)
                .map_err(VmError::NativeState)?;
        }
        Ok(())
    }

    fn pop_state_token(
        &mut self,
        operation: NativeStateOperation,
    ) -> Result<kira_runtime_abi::NativeStateToken, VmError> {
        let value = self.pop()?;
        match value {
            Value::NativeState(token) => Ok(token),
            Value::RawPtr(word) => Ok(kira_runtime_abi::NativeStateToken::from_word(word)),
            other => {
                self.heap.drop_value(other);
                Err(VmError::NativeStateValueMismatch {
                    operation,
                    kind: "a value that is neither callback state nor a token",
                })
            }
        }
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

    pub(super) fn native_state_release(&mut self) -> Result<(), VmError> {
        let token = self.pop_state_token(NativeStateOperation::Release)?;
        self.host
            .native_state_release(token)
            .map_err(VmError::NativeState)?;
        self.stack.push(Value::Void);
        Ok(())
    }
}
