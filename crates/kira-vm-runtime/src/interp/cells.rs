//! The three capture-cell primitives.
//!
//! A cell is the shared, mutable storage a `var` moves into when a closure
//! captures it — the one place this runtime lets two values name one mutable
//! object. Split out of [`super`] because the three arms are a cohesive unit
//! with one invariant between them, and because the interpreter's dispatch file
//! is already at its size ladder.
//!
//! # Why the get and set forms name a slot
//!
//! Every cell the compiler mints lives in a local: a boxed `var` is a local of
//! the frame that declared it, and a captured one is copied out of the
//! closure's representation struct into a local by the lifted body's prologue.
//! Naming the slot lets a read borrow the handle instead of taking a share of
//! it and dropping it again, exactly as `ArrayGetLocal` borrows an array.
//!
//! # Why a read is owned and a write is one step
//!
//! [`Instruction::CellGet`](kira_bytecode::op::Instruction::CellGet) hands back
//! a value the caller owns. A borrowing read would let a write through another
//! holder free the payload while a caller still had it — and a cell exists
//! precisely so that other holders exist.
//!
//! [`Instruction::CellSet`](kira_bytecode::op::Instruction::CellSet) replaces
//! the payload and releases the old one in a single step. Two steps would leave
//! the box holding a freed handle for the window between them.

use crate::error::VmError;
use crate::value::Value;

use super::Vm;
use super::frames::Frame;

impl Vm<'_> {
    /// `NewCell`: pop a value and push a fresh cell holding it.
    ///
    /// The value moves in. One box per execution, which is what makes a `var`
    /// declared inside a loop body a fresh binding on each turn — and what lets
    /// a closure made on one turn keep the storage of that turn.
    pub(super) fn new_cell(&mut self) -> Result<(), VmError> {
        let payload = self.pop()?;
        let id = self.heap.alloc_cell(payload);
        self.stack.push(Value::Cell(id));
        Ok(())
    }

    /// `CellGet`: push an owned copy of what the cell in `slot` holds.
    pub(super) fn cell_get(&mut self, frame: &Frame, slot: u16) -> Result<(), VmError> {
        // Borrowed, not consumed: the slot keeps its hold on the box, so
        // nothing is dropped here.
        let Some(Value::Cell(id)) = frame.locals.get(slot as usize).copied() else {
            return Err(VmError::NotACell);
        };
        let value = self.heap.cell_get(id).ok_or(VmError::NotACell)?;
        self.stack.push(value);
        Ok(())
    }

    /// `CellSet`: pop a value into the cell `slot` holds, releasing what was
    /// there.
    pub(super) fn cell_set(&mut self, frame: &Frame, slot: u16) -> Result<(), VmError> {
        let value = self.pop()?;
        let Some(Value::Cell(id)) = frame.locals.get(slot as usize).copied() else {
            // The popped value is this instruction's now, so a refusal releases
            // it rather than leaking it — the same discipline every other
            // consuming instruction follows on its error paths.
            self.heap.drop_value(value);
            return Err(VmError::NotACell);
        };
        if !self.heap.cell_set(id, value) {
            return Err(VmError::NotACell);
        }
        Ok(())
    }
}
