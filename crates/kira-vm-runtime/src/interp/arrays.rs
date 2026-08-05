//! The array instructions: building one, reading an element, counting, and
//! appending through a place.
//!
//! Split from the dispatch loop on the file-size ladder. They belong together
//! because they share one discipline: an array owns its elements, so every
//! path here either copies an element out before the array is dropped or drops
//! the array on every way out — including the failing ones. Reading them side
//! by side is what makes that checkable.
//!
//! Each also answers for a `NativeSnapshot`, which is an array-shaped view of
//! callback state rather than an array; the snapshot arms are here for the same
//! reason, so the two readings of "index into this" stay adjacent.

use kira_bytecode::op::PlacePath;
use kira_runtime_abi::NativeStatePathStep;

use crate::error::VmError;
use crate::value::Value;

use super::Vm;
use super::frames::Frame;
use super::place::check_index;

impl Vm<'_> {
    /// Pops `count` elements into a fresh array.
    pub(super) fn new_array(&mut self, count: u32) -> Result<(), VmError> {
        let first = self
            .stack
            .len()
            .checked_sub(count as usize)
            .ok_or(VmError::StackUnderflow)?;
        // The elements were pushed in written order, so splitting them off
        // preserves that order — and moves them, so nothing is copied and
        // nothing is left on the stack to double-free.
        let elements = self.stack.split_off(first);
        let id = self.heap.alloc_array(elements);
        self.stack.push(Value::Array(id));
        Ok(())
    }

    /// Pops an index and an array, pushes a copy of that element, drops the
    /// array.
    pub(super) fn array_get(&mut self) -> Result<(), VmError> {
        let index = self.pop_int()?;
        let base = self.pop()?;
        if let Value::NativeSnapshot(id) = base {
            let Ok(index) = u64::try_from(index) else {
                self.heap.free_snapshot(id);
                return Err(VmError::NegativeIndex);
            };
            let element = self.read_snapshot_child(
                id,
                NativeStatePathStep::Index(index),
                VmError::IndexOutOfBounds,
            )?;
            self.stack.push(element);
            return Ok(());
        }
        let Value::Array(id) = base else {
            self.heap.drop_value(base);
            return Err(VmError::NotAnArray);
        };
        let read = check_index(index, self.heap.array_len(id)).and_then(|index| {
            self.heap
                .element(id, index)
                .ok_or(VmError::IndexOutOfBounds)
        });
        let element = match read {
            Ok(element) => element,
            Err(error) => {
                // The array was ours; a failed read frees it.
                self.heap.drop_value(base);
                return Err(error);
            }
        };
        // The element is copied out before the array is dropped: the array owns
        // its elements, so handing one out without copying would hand out
        // storage this drop is about to free.
        let copy = self.heap.copy_value(element);
        self.heap.drop_value(base);
        self.stack.push(copy);
        Ok(())
    }

    /// Pops an index and pushes a copy of that element of the array in `slot`.
    ///
    /// The local is *borrowed*: its handle is read without copying the array,
    /// and nothing is dropped here because this instruction does not own it.
    /// Only the element is copied out, which is what keeps a handed-out element
    /// unshared.
    pub(super) fn array_get_local(&mut self, frame: &Frame, slot: u16) -> Result<(), VmError> {
        let index = self.pop_int()?;
        if let Value::NativeSnapshot(id) = frame.locals[slot as usize] {
            // Borrowed here too: the read stays in the local, so this takes a
            // hold of its own rather than consuming it.
            let Value::NativeSnapshot(borrowed) = self.heap.copy_value(Value::NativeSnapshot(id))
            else {
                return Err(VmError::NotAnArray);
            };
            let Ok(index) = u64::try_from(index) else {
                self.heap.free_snapshot(borrowed);
                return Err(VmError::NegativeIndex);
            };
            let element = self.read_snapshot_child(
                borrowed,
                NativeStatePathStep::Index(index),
                VmError::IndexOutOfBounds,
            )?;
            self.stack.push(element);
            return Ok(());
        }
        let Value::Array(id) = frame.locals[slot as usize] else {
            return Err(VmError::NotAnArray);
        };
        let index = check_index(index, self.heap.array_len(id))?;
        let element = self
            .heap
            .element(id, index)
            .ok_or(VmError::IndexOutOfBounds)?;
        let copy = self.heap.copy_value(element);
        self.stack.push(copy);
        Ok(())
    }

    /// Pops an array, pushes its element count, drops the array.
    pub(super) fn array_len(&mut self) -> Result<(), VmError> {
        let base = self.pop()?;
        if let Value::NativeSnapshot(id) = base {
            let len = self.snapshot_array_len(id)?;
            let counted = i64::try_from(len).map_err(|_| VmError::ArrayTooLong)?;
            self.stack.push(Value::Int(counted));
            return Ok(());
        }
        let Value::Array(id) = base else {
            self.heap.drop_value(base);
            return Err(VmError::NotAnArray);
        };
        let counted = self
            .heap
            .array_len(id)
            .ok_or(VmError::NotAnArray)
            .and_then(|len| i64::try_from(len).map_err(|_| VmError::ArrayTooLong));
        // The array is freed on every path out, not just the one that produced
        // a count.
        self.heap.drop_value(base);
        self.stack.push(Value::Int(counted?));
        Ok(())
    }

    /// Pops a value and appends it to the array the place names.
    pub(super) fn array_append(
        &mut self,
        frame: &mut Frame,
        slot: u16,
        path: &PlacePath,
    ) -> Result<(), VmError> {
        let value = self.pop()?;
        let appended = self.with_steps(|vm, steps| {
            vm.fill_steps(path, steps)?;
            vm.append_through(frame, slot, steps, value)
        });
        if let Err(error) = appended {
            self.heap.drop_value(value);
            return Err(error);
        }
        Ok(())
    }
}
