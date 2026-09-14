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

use kira_runtime_abi::{NativeStatePathStep, NativeStateToken, NativeStateValue};

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
            (Some(NativeStateValue::Struct(fields)), NativeStatePathStep::Field(index))
            | (
                Some(NativeStateValue::DropStruct { fields, .. }),
                NativeStatePathStep::Field(index),
            ) => fields.get(index as usize).cloned(),
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
        // A boxed value that runs a user `Drop` body records the body with the
        // store, so the release that destroys the state hands it back to be run
        // — the value-tree store cannot enter a Kira body itself. The glue is
        // read from the heap before the value is consumed into the tree.
        let glue = match value {
            Value::Struct(id) => self.heap.drop_glue_of(id),
            _ => None,
        };
        let stored = self.heap.into_native_state(value).map_err(|kind| {
            VmError::NativeStateValueMismatch {
                operation: NativeStateOperation::Store,
                kind,
            }
        })?;
        let token = self
            .host
            .native_state_create_dropping(type_id, stored, glue)
            .map_err(VmError::NativeState)?;
        self.stack.push(Value::NativeState(token));
        Ok(())
    }

    /// Releases one owner of `token`, and when that destroys a state carrying a
    /// user `Drop` body, rebuilds the value and parks it so the interpreter
    /// runs the body before the storage goes — the value-tree mirror of the
    /// native box free leaf.
    fn release_native_state_token(&mut self, token: NativeStateToken) -> Result<(), VmError> {
        let destroyed = self
            .host
            .native_state_release_dropping(token)
            .map_err(VmError::NativeState)?;
        if let Some(tree) = destroyed {
            // The portable tree carries Drop glue at every struct that owns a
            // body, including nested structs. Rebuild and release it through
            // ordinary VM destruction so bodies run in the normal order.
            self.heap.drop_native_state_value(tree);
        }
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
        if shared {
            // A borrowed export is a raw word only. `LoadLocal` created the
            // temporary stack owner above and recorded its retain, so dropping
            // that stack value balances the copy while the original local keeps
            // owning its reference. The raw word itself owns nothing.
            self.heap.drop_value(Value::NativeState(token));
            self.stack.push(Value::RawPtr(token.as_word()));
        } else {
            // The token owns one reference, and the value just popped is it: a
            // handle reaches this stack either as a temporary, which owns the
            // reference it was created with, or as a load, which the heap copied
            // and counted on the way out of its slot. Either way exactly one
            // reference arrives here and the token takes it over.
            //
            // The exported userdata is itself an affine owner: it keeps the
            // reference that arrived, is released when its binding goes out of
            // scope, and is counted when copied — so it can no longer be erased
            // into an untracked `RawPtr` word that nothing releases.
            self.stack.push(Value::NativeState(token));
        }
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
            self.release_native_state_token(token)?;
        }
        Ok(())
    }

    fn pop_state_token(
        &mut self,
        operation: NativeStateOperation,
    ) -> Result<NativeStateToken, VmError> {
        let value = self.pop()?;
        match value {
            Value::NativeState(token) => Ok(token),
            Value::RawPtr(word) => Ok(NativeStateToken::from_word(word)),
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
        // A recovery *borrows*: the argument is either a raw token (no reference
        // to give back) or an owned handle read onto the stack, which the load
        // retained. A borrow keeps the caller's owner intact, so the retained
        // reference the load took is released here — the view owns nothing.
        let (token, release) = match raw {
            Value::RawPtr(word) => (NativeStateToken::from_word(word), false),
            Value::NativeState(token) => (token, true),
            other => {
                self.heap.drop_value(other);
                return Err(VmError::NativeStateValueMismatch {
                    operation: NativeStateOperation::Recover,
                    kind: "a value that is not a callback-state token",
                });
            }
        };
        let type_id = kira_runtime_abi::NativeStateTypeId::new(type_word);
        // Check the token and type, and read nothing: what goes on the stack is
        // a handle. This used to recover the state — a deep copy of everything
        // it holds, discarded immediately — on every recovery, which is once per
        // function that touches it and many times per frame.
        self.host
            .native_state_check(token, type_id)
            .map_err(VmError::NativeState)?;
        if release {
            self.host
                .native_state_release(token)
                .map_err(VmError::NativeState)?;
        }
        self.stack.push(Value::NativeView { token, type_id });
        Ok(())
    }

    /// Takes the whole state out as a value and gives up the token.
    ///
    /// [`Self::native_recover`] pushes a view and reads nothing, so that
    /// reading one field out of a large state does not rebuild every string
    /// and array beside it. A caller that needs the value itself — a channel
    /// handing a payload to its receiver — needs the other answer, and needs
    /// the token released in the same breath: it was the queue's, and the
    /// queue no longer holds the slot it named.
    pub(super) fn native_state_take(&mut self, type_word: u64) -> Result<(), VmError> {
        let raw = self.pop()?;
        let Value::RawPtr(word) = raw else {
            self.heap.drop_value(raw);
            return Err(VmError::NativeStateValueMismatch {
                operation: NativeStateOperation::Recover,
                kind: "a value that is not a callback-state token",
            });
        };
        let token = NativeStateToken::from_word(word);
        let type_id = kira_runtime_abi::NativeStateTypeId::new(type_word);
        let tree = self
            .host
            .native_state_recover(token, type_id)
            .map_err(VmError::NativeState)?;
        let value = self.heap.from_native_state(&tree);
        self.host
            .native_state_release(token)
            .map_err(VmError::NativeState)?;
        self.stack.push(value);
        Ok(())
    }

    /// Releases the storage every undelivered channel payload still names.
    ///
    /// A run can end with values still queued — an early return, a trap, a
    /// receiver that stopped taking — and a queued word of a boxed channel is
    /// a token naming storage in a store that outlives the run. In a hybrid
    /// session that store is the native half's, which outlives the process, so
    /// a run that dropped its table would leak once per run.
    ///
    /// Errors are not reported. This runs while a run is being torn down, and
    /// a token the store no longer knows is one somebody already released:
    /// there is nothing left to tell and nobody to tell it to.
    pub(crate) fn release_undelivered_channel_payloads(&mut self) {
        for token in self.channels.take_undelivered_tokens() {
            let _ = self
                .host
                .native_state_release(NativeStateToken::from_word(token as u64));
        }
    }

    pub(super) fn native_state_release(&mut self) -> Result<(), VmError> {
        let token = self.pop_state_token(NativeStateOperation::Release)?;
        self.release_native_state_token(token)?;
        self.stack.push(Value::Void);
        Ok(())
    }
}
