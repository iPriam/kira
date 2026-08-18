//! Arrays on the heap: uniqueness, element access, growth, and release.
//!
//! Separate from the other object kinds because an array is the one that shares
//! its buffer — `make_array_unique` is the copy-on-write seam, and every reader
//! below it has to know whether it is looking at a shared buffer or its own.

use super::*;

impl Heap {
    /// Gives an array elements of its own, so a write reaches nothing else.
    ///
    /// The deferred half of [`Heap::copy_value`]: an array that shares its
    /// elements copies them here, deeply, on the first write through *this*
    /// handle. Sole ownership — the common case — is a count and a compare.
    ///
    /// Every write goes through this: an element store, an append, and each
    /// index step of a place walk, since a walk that passes through an array
    /// reads a handle *out of* it and is about to write into whatever that
    /// handle names.
    pub fn make_array_unique(&mut self, id: ArrayId) {
        let shared = match self.slots.get(id.0 as usize) {
            Some(Some(Object::Array(elements))) if Rc::strong_count(elements) > 1 => {
                Rc::clone(elements)
            }
            _ => return,
        };
        let mut copies = Vec::with_capacity(shared.len());
        for element in shared.iter() {
            copies.push(self.copy_value(*element));
        }
        if let Some(Some(Object::Array(elements))) = self.slots.get_mut(id.0 as usize) {
            *elements = Rc::new(copies);
        }
    }

    /// Borrows the elements of the array behind a handle.
    ///
    /// Returns an empty slice for a handle whose slot was already freed or
    /// holds something else, so a misbehaving caller cannot panic the VM.
    pub fn elements(&self, id: ArrayId) -> &[Value] {
        match self.slots.get(id.0 as usize) {
            Some(Some(Object::Array(elements))) => elements,
            _ => &[],
        }
    }

    /// The number of elements in an array, or `None` when the handle does not
    /// name one.
    pub fn array_len(&self, id: ArrayId) -> Option<usize> {
        match self.slots.get(id.0 as usize) {
            Some(Some(Object::Array(elements))) => Some(elements.len()),
            _ => None,
        }
    }

    /// The value at `index` of an array, or `None` when the handle does not
    /// name an array with that many elements.
    pub fn element(&self, id: ArrayId, index: usize) -> Option<Value> {
        self.elements(id).get(index).copied()
    }

    /// Replaces the element at `index`, dropping what was there, and reports
    /// whether the element existed.
    ///
    /// Returns `false` — having changed nothing and dropped nothing — when the
    /// handle does not name an array with that element.
    pub fn set_element(&mut self, id: ArrayId, index: usize, value: Value) -> bool {
        // Bounds first, so an index that is about to trap copies nothing and
        // takes no ownership; then the copy, and only then the read of what is
        // being replaced. In that order the value dropped below is this array's
        // own — read before the copy it would be the one every sharer still
        // holds. `own` has to come after the bounds check for the same reason
        // it does in `set_field`: it allocates and consumes the caller's
        // snapshot slot, so a refusal after it leaks both.
        if !matches!(self.array_len(id), Some(len) if index < len) {
            return false;
        }
        let value = self.own(value);
        self.make_array_unique(id);
        let Some(previous) = self.element(id, index) else {
            // Unreachable given the bounds check, but the owned value would be
            // ours to release if unsharing ever left the element unreadable.
            self.drop_value(value);
            return false;
        };
        self.drop_value(previous);
        match self.slots.get_mut(id.0 as usize) {
            // Sole owner by now, so this hands back the `Vec` itself.
            Some(Some(Object::Array(elements))) => match Rc::make_mut(elements).get_mut(index) {
                Some(slot) => {
                    *slot = value;
                    true
                }
                None => false,
            },
            _ => false,
        }
    }

    /// Pushes `value` onto the end of an array, reporting whether the handle
    /// named one.
    ///
    /// The array grows in place: this is the one operation whose whole purpose
    /// is that the *caller's* array changes, which is why `append` resolves a
    /// place rather than taking a value.
    pub fn push_element(&mut self, id: ArrayId, value: Value) -> bool {
        // That the handle names an array is checked before ownership is taken:
        // the match below can still answer `false`, and reaching that after
        // `own` would leak the objects it allocated and leave the caller with a
        // consumed snapshot slot it thinks is still live.
        if self.array_len(id).is_none() {
            return false;
        }
        let value = self.own(value);
        // An append is a write like any other, so it lengthens this array's own
        // elements rather than the ones it was sharing.
        self.make_array_unique(id);
        match self.slots.get_mut(id.0 as usize) {
            Some(Some(Object::Array(elements))) => {
                // Sole owner by now, so this hands back the `Vec` itself and
                // copies nothing.
                Rc::make_mut(elements).push(value);
                true
            }
            _ => false,
        }
    }

    /// Frees the array behind a handle, dropping its elements once the last
    /// array holding them lets go.
    ///
    /// The slot always goes, so the accounting balances per handle exactly as
    /// it did when every copy was deep. The elements go with the last of them,
    /// which is what keeps a shared element freed once rather than once per
    /// sharer.
    ///
    /// Bounded by the program's nesting depth for the same reason
    /// [`Heap::free_struct`] is: an array's element type is resolved during
    /// analysis against types that already resolve, so a cycle is
    /// unrepresentable.
    pub fn free_array(&mut self, id: ArrayId) {
        let taken = match self.slots.get_mut(id.0 as usize) {
            Some(slot @ Some(Object::Array(_))) => slot.take(),
            _ => None,
        };
        let Some(Object::Array(elements)) = taken else {
            return;
        };
        self.freed += 1;
        self.free_list.push(id.0);
        // Another array still reads these, so none of what they own is released
        // here — only this handle's claim on them.
        let Ok(elements) = Rc::try_unwrap(elements) else {
            return;
        };
        for element in elements {
            self.drop_value(element);
        }
    }
}
