//! Walking and writing a place: the field/index paths behind `StoreField`,
//! `StorePlace`, and `ArrayAppend`.
//!
//! A place is a local slot optionally walked into by fields and array indices.
//! Walking moves *handles*, never objects — each step reads the nested object's
//! handle out of its parent — so a write through the result lands in the object
//! the local holds rather than rebuilding it. That is only sound because every
//! object is exclusively owned by the time it is written through.
//!
//! An array is the one kind that may not be: copying one shares its elements
//! until somebody writes. Every walk in this module is a walk *to a write*, so
//! each index step takes the array's elements over first
//! ([`crate::value::Heap::make_array_unique`]) and only then reads the handle
//! out of them — otherwise the handle it read would name an object another
//! array is still reading, and the write would land in both.

use kira_bytecode::op::{PathStep, PlacePath};

use super::{Frame, Vm};
use crate::error::{NativeStateOperation, VmError};
use crate::value::Value;
use kira_runtime_abi::NativeStatePathStep;

/// A place step with its index value already taken off the stack.
///
/// The bytecode's [`PathStep`] says only *that* a step indexes; this says
/// *what* it indexes. Resolving the whole path before walking it is what keeps
/// the stack discipline in one place instead of interleaved with the walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ResolvedStep {
    /// Walk into the field at this index.
    Field(u16),
    /// Walk into the element at this index, as the program computed it.
    Index(i64),
}

/// Turns a signed index into a usable one, or names which trap it is.
///
/// A negative index and one past the end are **different mistakes**, and they
/// get different traps: a negative index is a computation that went wrong,
/// while an index past the end is a length that was misjudged. Collapsing them
/// into one message would lose that.
///
/// `len` is `None` when the handle does not name an array at all.
pub(super) fn check_index(index: i64, len: Option<usize>) -> Result<usize, VmError> {
    let Ok(index) = usize::try_from(index) else {
        return Err(VmError::NegativeIndex);
    };
    match len {
        Some(len) if index < len => Ok(index),
        Some(_) => Err(VmError::IndexOutOfBounds),
        None => Err(VmError::NotAnArray),
    }
}

impl Vm<'_> {
    /// Walks `steps` from local `slot`, returning the value the last step lands
    /// *on* — that is, the value the caller is about to write into.
    fn walk_place(
        &mut self,
        frame: &Frame,
        slot: u16,
        steps: &[ResolvedStep],
    ) -> Result<Value, VmError> {
        self.walk_value(frame.locals[slot as usize], steps)
    }

    /// Rebuilds a local that holds a deferred state read, so a write can land.
    ///
    /// This is where a read stops being deferred. A read of callback state hands
    /// back the stored node rather than objects, which is sound only while
    /// nobody writes to it: the local was bound by a *copy*, so a write through
    /// it must reach the copy and not the state. Rebuilding here is that copy,
    /// paid at the first write instead of at every read — and a local that is
    /// only ever read never pays it at all.
    fn own_local(&mut self, frame: &mut Frame, slot: u16) {
        let local = frame.locals[slot as usize];
        if matches!(local, Value::NativeSnapshot(_)) {
            frame.locals[slot as usize] = self.heap.own(local);
        }
    }

    fn walk_value(&mut self, mut current: Value, steps: &[ResolvedStep]) -> Result<Value, VmError> {
        for step in steps {
            current = self.walk_step(current, *step)?;
        }
        Ok(current)
    }

    /// Takes one step of a place walk, on the way to a write.
    fn walk_step(&mut self, current: Value, step: ResolvedStep) -> Result<Value, VmError> {
        match step {
            ResolvedStep::Field(index) => {
                let Value::Struct(id) = current else {
                    return Err(VmError::NotAStruct);
                };
                self.heap
                    .field(id, index)
                    .ok_or(VmError::NoSuchField { index })
            }
            ResolvedStep::Index(index) => {
                let Value::Array(id) = current else {
                    return Err(VmError::NotAnArray);
                };
                let index = check_index(index, self.heap.array_len(id))?;
                // The bounds check first, so an index that is about to trap
                // copies nothing; then the elements become this array's own,
                // and only then is the handle read out of them.
                self.heap.make_array_unique(id);
                self.heap
                    .element(id, index)
                    .ok_or(VmError::IndexOutOfBounds)
            }
        }
    }

    /// Writes `value` through a dynamic place rooted at `slot`.
    ///
    /// The last step is where the write lands; every step before it is a walk.
    pub(super) fn store_place(
        &mut self,
        frame: &mut Frame,
        slot: u16,
        steps: &[ResolvedStep],
        value: Value,
    ) -> Result<(), VmError> {
        self.own_local(frame, slot);
        if let Value::NativeView { token, type_id } = frame.locals[slot as usize] {
            if steps.is_empty() {
                self.heap.drop_value(value);
                return Err(VmError::EmptyFieldPath);
            }
            return self.write_through_view(token, type_id, steps, value);
        }
        let Some((&last, walk)) = steps.split_last() else {
            return Err(VmError::EmptyFieldPath);
        };
        let target = self.walk_place(frame, slot, walk)?;
        self.write_last(target, last, value)
    }

    /// Writes `value` through a field-only place (`StoreField`), walking the
    /// `&[u16]` field path directly.
    ///
    /// This is the shape that predates arrays, and it stays allocation-free: a
    /// field step needs no stack index, so nothing is resolved into a scratch
    /// buffer — the walk reads the constant indices straight off the path.
    pub(super) fn store_field(
        &mut self,
        frame: &mut Frame,
        slot: u16,
        path: &[u16],
        value: Value,
    ) -> Result<(), VmError> {
        self.own_local(frame, slot);
        if let Value::NativeView { token, type_id } = frame.locals[slot as usize] {
            if path.is_empty() {
                self.heap.drop_value(value);
                return Err(VmError::EmptyFieldPath);
            }
            self.native_path.clear();
            self.native_path.extend(
                path.iter()
                    .map(|&index| NativeStatePathStep::Field(index.into())),
            );
            return self.write_native_path(token, type_id, value, false);
        }
        let Some((&last, walk)) = path.split_last() else {
            return Err(VmError::EmptyFieldPath);
        };
        let mut target = frame.locals[slot as usize];
        for &index in walk {
            target = self.walk_step(target, ResolvedStep::Field(index))?;
        }
        self.write_last(target, ResolvedStep::Field(last), value)
    }

    /// Writes `value` into the location the last step names.
    fn write_last(
        &mut self,
        target: Value,
        last: ResolvedStep,
        value: Value,
    ) -> Result<(), VmError> {
        match last {
            ResolvedStep::Field(index) => {
                let Value::Struct(id) = target else {
                    return Err(VmError::NotAStruct);
                };
                if !self.heap.set_field(id, index, value) {
                    return Err(VmError::NoSuchField { index });
                }
            }
            ResolvedStep::Index(index) => {
                let Value::Array(id) = target else {
                    return Err(VmError::NotAnArray);
                };
                let index = check_index(index, self.heap.array_len(id))?;
                if !self.heap.set_element(id, index, value) {
                    return Err(VmError::IndexOutOfBounds);
                }
            }
        }
        Ok(())
    }

    /// Appends `value` to the array the place rooted at `slot` names.
    ///
    /// Unlike a store, **every** step is a walk: the place names the array
    /// itself, not a slot inside it. An empty path appends to the local's own
    /// array, which is what `xs.append(v)` compiles to.
    pub(super) fn append_through(
        &mut self,
        frame: &mut Frame,
        slot: u16,
        steps: &[ResolvedStep],
        value: Value,
    ) -> Result<(), VmError> {
        self.own_local(frame, slot);
        if let Value::NativeView { token, type_id } = frame.locals[slot as usize] {
            return self.write_through_view_appending(token, type_id, steps, value);
        }
        let target = self.walk_place(frame, slot, steps)?;
        let Value::Array(id) = target else {
            return Err(VmError::NotAnArray);
        };
        if !self.heap.push_element(id, value) {
            return Err(VmError::NotAnArray);
        }
        Ok(())
    }

    /// Writes `value` at `steps` inside callback state, addressing it by path.
    ///
    /// The state is never materialized as VM objects to do this. It used to be:
    /// a field write recovered the whole state, rebuilt every string, array and
    /// struct in it as heap objects, wrote one field, and boxed all of it back.
    /// That is O(state) per write, so a UI batch carrying a glyph cache paid for
    /// the cache on every `quadCount = quadCount + 1` — which is what made the
    /// VM unusably slow on a real UI. Addressing by path costs the depth of the
    /// path instead.
    fn write_through_view(
        &mut self,
        token: kira_runtime_abi::NativeStateToken,
        type_id: kira_runtime_abi::NativeStateTypeId,
        steps: &[ResolvedStep],
        value: Value,
    ) -> Result<(), VmError> {
        self.resolve_native_path(steps)?;
        self.write_native_path(token, type_id, value, false)
    }

    /// Appends `value` to the array `steps` addresses inside callback state.
    fn write_through_view_appending(
        &mut self,
        token: kira_runtime_abi::NativeStateToken,
        type_id: kira_runtime_abi::NativeStateTypeId,
        steps: &[ResolvedStep],
        value: Value,
    ) -> Result<(), VmError> {
        self.resolve_native_path(steps)?;
        self.write_native_path(token, type_id, value, true)
    }

    /// Fills the reusable path buffer from resolved place steps.
    ///
    /// Reused rather than allocated: this runs on every write through callback
    /// state, which is every mutation of a UI batch's counters.
    fn resolve_native_path(&mut self, steps: &[ResolvedStep]) -> Result<(), VmError> {
        self.native_path.clear();
        for step in steps {
            self.native_path.push(match *step {
                ResolvedStep::Field(index) => NativeStatePathStep::Field(index.into()),
                ResolvedStep::Index(index) => NativeStatePathStep::Index(
                    u64::try_from(index).map_err(|_| VmError::NegativeIndex)?,
                ),
            });
        }
        Ok(())
    }

    /// Boxes `value` and hands it to the host at the resolved path.
    fn write_native_path(
        &mut self,
        token: kira_runtime_abi::NativeStateToken,
        type_id: kira_runtime_abi::NativeStateTypeId,
        value: Value,
        appending: bool,
    ) -> Result<(), VmError> {
        let stored = self.heap.into_native_state(value).map_err(|kind| {
            VmError::NativeStateValueMismatch {
                operation: NativeStateOperation::Store,
                kind,
            }
        })?;
        let path = std::mem::take(&mut self.native_path);
        let outcome = if appending {
            self.host.native_state_append(token, type_id, &path, stored)
        } else {
            self.host.native_state_write(token, type_id, &path, stored)
        };
        self.native_path = path;
        outcome.map_err(VmError::NativeState)
    }

    /// Fills `buf` with `path`'s steps, popping one index value per `Index`
    /// step, outermost first.
    ///
    /// The indices were pushed outermost-first and the *value* was pushed last,
    /// so by the time this runs the value is already off and the indices come
    /// off innermost-first — hence the reverse walk. This is the other half of
    /// the contract `kira_ir::IrPlace` states.
    ///
    /// `buf` is the VM's reusable scratch (taken out with `mem::take` by the
    /// caller), so filling it reuses its capacity rather than allocating per op.
    pub(super) fn fill_steps(
        &mut self,
        path: &PlacePath,
        buf: &mut Vec<ResolvedStep>,
    ) -> Result<(), VmError> {
        buf.clear();
        buf.resize(path.steps().len(), ResolvedStep::Field(0));
        for (slot, step) in buf.iter_mut().zip(path.steps().iter()).rev() {
            *slot = match step {
                PathStep::Field(index) => ResolvedStep::Field(*index),
                PathStep::Index => ResolvedStep::Index(self.pop_int()?),
            };
        }
        Ok(())
    }

    /// Runs `body` with the reusable step buffer taken out of the VM, then hands
    /// it back cleared for the next op — capacity kept, never freed.
    ///
    /// Taking it out is what lets `body` both fill the buffer and pop the
    /// operand stack: the two would otherwise be a double mutable borrow of the
    /// VM.
    pub(super) fn with_steps<R>(
        &mut self,
        body: impl FnOnce(&mut Self, &mut Vec<ResolvedStep>) -> R,
    ) -> R {
        let mut steps = std::mem::take(&mut self.steps);
        let result = body(self, &mut steps);
        steps.clear();
        self.steps = steps;
        result
    }
}
