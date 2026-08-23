//! Cells: the one heap object whose identity outlives the value inside it.
//!
//! A cell is what a captured variable and a native-held reference are, so its
//! release is deferred through `ReleasedCells` rather than immediate — the
//! native half may still be holding the handle when the VM lets go.

use super::*;

impl Heap {
    /// Boxes `payload` into a fresh capture cell, returning its handle.
    ///
    /// The payload is taken rather than copied: whatever produced it (the
    /// operand stack) hands over ownership, exactly as an enum's payload is
    /// handed over.
    pub fn alloc_cell(&mut self, payload: Value) -> CellId {
        let payload = self.own(payload);
        CellId(self.alloc_object(Object::Cell { payload, shares: 1 }))
    }

    /// An **owned** copy of what a capture cell holds.
    ///
    /// `None` when the handle does not name a cell. The copy answers to
    /// [`Heap::copy_value`], so the caller owns what it gets back and the box
    /// keeps owning its own: a later write through the cell cannot free a value
    /// a reader is still holding.
    pub fn cell_get(&mut self, id: CellId) -> Option<Value> {
        let payload = match self.slots.get(id.0 as usize) {
            Some(Some(Object::Cell { payload, .. })) => *payload,
            _ => return None,
        };
        Some(self.copy_value(payload))
    }

    /// Replaces what a capture cell holds, dropping what was there, and reports
    /// whether the handle named one.
    ///
    /// **One step.** The old payload is taken out of the box *before* it is
    /// dropped, so nothing can observe the box holding storage that is being
    /// released; the new value is in place before the drop runs. A drop
    /// followed by a store would leave a freed handle readable in between, and
    /// a trap in that window would leave it there for good.
    ///
    /// This consumes `value` on every path out, refusals included: a caller
    /// that handed a value over no longer owns it, whatever the answer was.
    /// A value that *is* the cell being written is refused outright — storing
    /// a box inside itself builds a share cycle no count can collect.
    pub fn cell_set(&mut self, id: CellId, value: Value) -> bool {
        // Validated before ownership is taken: `own` can allocate, and either
        // refusal below must release the incoming value rather than strand an
        // owned copy of it.
        let named = matches!(
            self.slots.get(id.0 as usize),
            Some(Some(Object::Cell { .. }))
        );
        if match value {
            Value::Cell(self_id) => self_id == id,
            _ => false,
        } || !named
        {
            self.drop_value(value);
            return false;
        }
        let value = self.own(value);
        let previous = match self.slots.get_mut(id.0 as usize) {
            Some(Some(Object::Cell { payload, .. })) => std::mem::replace(payload, value),
            _ => return false,
        };
        self.drop_value(previous);
        true
    }

    /// Hands one hold on a capture cell to a callback-state tree.
    ///
    /// The hold is the caller's, transferred rather than duplicated: everything
    /// that goes into a state tree is *moved* there, and a cell moves the same
    /// way. What the tree gets is the handle plus the release that puts the hold
    /// back, which it runs when the last clone of the node goes.
    pub fn cell_into_native_state(&mut self, id: CellId) -> kira_runtime_abi::NativeCell {
        let released = Arc::clone(&self.released_cells);
        kira_runtime_abi::NativeCell::from_vm(u64::from(id.0), move |handle| {
            let mut handles = match released.handles.lock() {
                Ok(handles) => handles,
                // A panic while the list was held leaves the list itself intact:
                // it is a `Vec<u32>` and every write to it is one push. Dropping
                // this release instead would leak the cell.
                Err(poisoned) => poisoned.into_inner(),
            };
            handles.push(handle as u32);
            // Raised while the lock is held, so a drain that sees a count sees
            // the handle it counts.
            released.pending.fetch_add(1, Ordering::Release);
        })
    }

    /// Releases every cell a callback-state tree has finished with.
    ///
    /// Runs once per instruction, so a release is never more than one
    /// instruction late. The idle cost is the relaxed load that finds nothing.
    pub fn drain_released_cells(&mut self) {
        while self.released_cells.pending.load(Ordering::Acquire) > 0 {
            let batch = {
                let mut handles = match self.released_cells.handles.lock() {
                    Ok(handles) => handles,
                    Err(poisoned) => poisoned.into_inner(),
                };
                self.released_cells
                    .pending
                    .fetch_sub(handles.len(), Ordering::Release);
                std::mem::take(&mut *handles)
            };
            // Freeing one of these can free a snapshot holding a tree node,
            // which records more releases, so this drains until nothing is left
            // rather than once.
            for handle in batch {
                self.free_cell(CellId(handle));
            }
        }
    }

    /// Releases one hold on a capture cell, dropping its payload with the last.
    ///
    /// Bounded by the program's nesting depth for every value a cell can hold
    /// *except* an indirect share cycle: a cell whose payload reaches it again
    /// through a struct or erased box cannot be collected by counts. Storing a
    /// box in itself is refused at [`Heap::cell_set`]; the indirect form is
    /// memory-safe — never freed twice, never freed early — but strands the
    /// box, and collecting it needs either weak handles or a tracing pass,
    /// neither of which this runtime has.
    pub fn free_cell(&mut self, id: CellId) {
        // Another value still holds this box, so nothing it owns goes here.
        if let Some(Some(Object::Cell { shares, .. })) = self.slots.get_mut(id.0 as usize)
            && *shares > 1
        {
            *shares -= 1;
            return;
        }
        let taken = match self.slots.get_mut(id.0 as usize) {
            Some(slot @ Some(Object::Cell { .. })) => slot.take(),
            _ => None,
        };
        let Some(Object::Cell { payload, .. }) = taken else {
            return;
        };
        self.freed += 1;
        self.free_list.push(id.0);
        self.drop_value(payload);
    }
}
