//! Walking and writing a place: the field/index paths behind `StoreField`,
//! `StorePlace`, and `ArrayAppend`.
//!
//! A place is a local slot optionally walked into by fields and array indices.
//! Walking moves *handles*, never objects — each step reads the nested object's
//! handle out of its parent — so a write through the result lands in the object
//! the local holds rather than rebuilding it. That is only sound because every
//! object is exclusively owned: the deep copy on every read guarantees no other
//! value shares it.

use kira_bytecode::op::{PathStep, PlacePath};

use super::{Frame, Vm};
use crate::error::VmError;
use crate::value::Value;

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
        &self,
        frame: &Frame,
        slot: u16,
        steps: &[ResolvedStep],
    ) -> Result<Value, VmError> {
        let mut current = frame.locals[slot as usize];
        for step in steps {
            current = self.walk_step(current, *step)?;
        }
        Ok(current)
    }

    /// Takes one step of a place walk.
    fn walk_step(&self, current: Value, step: ResolvedStep) -> Result<Value, VmError> {
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
        let target = self.walk_place(frame, slot, steps)?;
        let Value::Array(id) = target else {
            return Err(VmError::NotAnArray);
        };
        if !self.heap.push_element(id, value) {
            return Err(VmError::NotAnArray);
        }
        Ok(())
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
