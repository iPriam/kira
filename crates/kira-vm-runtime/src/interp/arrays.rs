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
use kira_runtime_abi::{ForeignType, NativeStatePathStep, NativeStateValue};

use crate::error::VmError;
use crate::value::Value;

use super::Vm;
use super::frames::Frame;
use super::place::check_index;

impl Vm<'_> {
    /// Pops `count` elements into a fresh array.
    pub(super) fn new_array(&mut self, count: u64) -> Result<(), VmError> {
        let count = usize::try_from(count).map_err(|_| VmError::ArrayTooLong)?;
        let first = self
            .stack
            .len()
            .checked_sub(count)
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
            if !matches!(
                self.heap.snapshot_node(id),
                Some(NativeStateValue::Array(_))
            ) {
                self.heap.free_snapshot(id);
                return Err(VmError::NotAnArray);
            }
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
    pub(super) fn array_get_local(&mut self, frame: &Frame, slot: u64) -> Result<(), VmError> {
        let slot = usize::try_from(slot).map_err(|_| VmError::LocalSlotOutOfRange(slot))?;
        let index = self.pop_int()?;
        let local = frame
            .locals
            .get(slot)
            .copied()
            .ok_or(VmError::LocalSlotOutOfRange(slot as u64))?;
        if let Value::NativeSnapshot(id) = local {
            if !matches!(
                self.heap.snapshot_node(id),
                Some(NativeStateValue::Array(_))
            ) {
                return Err(VmError::NotAnArray);
            }
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
        let Value::Array(id) = local else {
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

    /// Pops an array-shaped value and retains its elements in C seam widths.
    ///
    /// A recovered native-state array is a snapshot rather than a heap array,
    /// but it has the same read-only seam meaning and must cross the boundary
    /// without first becoming an editable heap object.
    pub(super) fn array_elements(&mut self, element: ForeignType) -> Result<(), VmError> {
        let value = self.pop()?;
        let mut bytes = Vec::new();
        let result = match value {
            Value::Array(id) => {
                let result = self
                    .heap
                    .elements(id)
                    .iter()
                    .try_for_each(|item| super::write_seam_scalar(&mut bytes, element, *item));
                self.heap.drop_value(Value::Array(id));
                result
            }
            Value::NativeSnapshot(id) => {
                let result = match self.heap.snapshot_node(id) {
                    Some(NativeStateValue::Array(elements)) => {
                        elements.iter().try_for_each(|item| {
                            write_native_state_seam_scalar(&mut bytes, element, item)
                        })
                    }
                    _ => Err(VmError::NotAnArray),
                };
                self.heap.free_snapshot(id);
                result
            }
            other => {
                self.heap.drop_value(other);
                Err(VmError::NotAnArray)
            }
        };
        result?;
        // An owned block: the flattened elements live as long as the value
        // holding them — a struct's pointer member, or a call argument dropped
        // when the call returns. An empty array crosses as C's null, which no
        // element row has an address for.
        self.stack.push(if bytes.is_empty() {
            Value::RawPtr(0)
        } else {
            let block = self.heap.cblock_bytes(bytes);
            Value::CBlock(block)
        });
        Ok(())
    }

    /// Pops a value and appends it to the array the place names.
    pub(super) fn array_append(
        &mut self,
        frame: &mut Frame,
        slot: u64,
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

fn write_native_state_seam_scalar(
    out: &mut Vec<u8>,
    ty: ForeignType,
    value: &NativeStateValue,
) -> Result<(), VmError> {
    let mismatch = VmError::TypeMismatch {
        expected: "an array element the C seam can carry",
    };
    match (ty, value) {
        (ForeignType::I8, NativeStateValue::Int(n)) => out.push(*n as u8),
        (ForeignType::U8 | ForeignType::Bool, NativeStateValue::Int(n)) => out.push(*n as u8),
        (ForeignType::Bool, NativeStateValue::Bool(flag)) => out.push(u8::from(*flag)),
        (ForeignType::I16 | ForeignType::U16, NativeStateValue::Int(n)) => {
            out.extend_from_slice(&(*n as u16).to_le_bytes());
        }
        (ForeignType::I32 | ForeignType::U32, NativeStateValue::Int(n)) => {
            out.extend_from_slice(&(*n as u32).to_le_bytes());
        }
        (ForeignType::I64 | ForeignType::U64, NativeStateValue::Int(n)) => {
            out.extend_from_slice(&n.to_le_bytes());
        }
        (ForeignType::F32, NativeStateValue::Float(x)) => {
            out.extend_from_slice(&(*x as f32).to_le_bytes());
        }
        (ForeignType::F64, NativeStateValue::Float(x)) => {
            out.extend_from_slice(&x.to_le_bytes());
        }
        (ForeignType::RawPtr, NativeStateValue::RawPtr(word)) => {
            out.extend_from_slice(&word.to_le_bytes());
        }
        _ => return Err(mismatch),
    }
    Ok(())
}
