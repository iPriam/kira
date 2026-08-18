//! Structs and the object table underneath every heap value.
//!
//! `alloc_object` is where every kind above lands, so this module holds both the
//! table itself and the struct operations that are nothing more than field
//! access over it.

use super::*;

impl Heap {
    pub(super) fn alloc_object(&mut self, object: Object) -> u32 {
        self.allocated += 1;
        if let Some(index) = self.free_list.pop() {
            self.slots[index as usize] = Some(object);
            index
        } else {
            let index = self.slots.len() as u32;
            self.slots.push(Some(object));
            index
        }
    }

    /// Borrows the string behind a handle.
    ///
    /// Returns `""` for a handle whose slot was already freed or holds a
    /// struct, so a misbehaving caller cannot panic the VM.
    pub fn get(&self, id: StrId) -> &str {
        match self.slots.get(id.0 as usize) {
            Some(Some(Object::Str(text))) => text,
            _ => "",
        }
    }

    /// Borrows the fields of the struct behind a handle.
    ///
    /// Returns an empty slice for a handle whose slot was already freed or
    /// holds a string; the interpreter turns that into a typed trap rather
    /// than reading a field that is not there.
    pub fn fields(&self, id: StructId) -> &[Value] {
        match self.slots.get(id.0 as usize) {
            Some(Some(Object::Struct(fields))) => fields,
            _ => &[],
        }
    }

    /// The value of field `index` of a struct, or `None` when the handle does
    /// not name a struct with that many fields.
    pub fn field(&self, id: StructId, index: u64) -> Option<Value> {
        usize::try_from(index)
            .ok()
            .and_then(|index| self.fields(id).get(index))
            .copied()
    }

    /// Replaces field `index` of a struct, dropping what was there, and
    /// reports whether the field existed.
    ///
    /// Returns `false` — having changed nothing and dropped nothing — when the
    /// handle does not name a struct with that field.
    pub fn set_field(&mut self, id: StructId, index: u64, value: Value) -> bool {
        let value = self.own(value);
        if self.field(id, index).is_none() {
            return false;
        }
        // Fields of this struct's own before anything lands in them: a write
        // through one handle must not be visible through another that copied it.
        self.make_struct_unique(id);
        // Read *after* unsharing. The handle in the shared block belongs to
        // every holder of it; dropping that one would free the storage the other
        // holders are still reading.
        let Some(previous) = self.field(id, index) else {
            return false;
        };
        self.drop_value(previous);
        match self.slots.get_mut(id.0 as usize) {
            Some(Some(Object::Struct(fields))) => match Rc::get_mut(fields) {
                // Unique by the call above, so this is the writer's own block.
                Some(fields) => match usize::try_from(index)
                    .ok()
                    .and_then(|index| fields.get_mut(index))
                {
                    Some(slot) => {
                        *slot = value;
                        true
                    }
                    None => false,
                },
                None => false,
            },
            _ => false,
        }
    }

    /// Gives a struct fields of its own, if it is sharing them.
    ///
    /// The counterpart of [`Heap::make_array_unique`], and the same shape: the
    /// block moves, the handle does not, so every other holder of this id keeps
    /// reading the block it already had and no holder has to be found.
    pub fn make_struct_unique(&mut self, id: StructId) {
        let shared = match self.slots.get(id.0 as usize) {
            Some(Some(Object::Struct(fields))) if Rc::strong_count(fields) > 1 => Rc::clone(fields),
            _ => return,
        };
        let mut copies = Vec::with_capacity(shared.len());
        for field in shared.iter() {
            copies.push(self.copy_value(*field));
        }
        if let Some(Some(Object::Struct(fields))) = self.slots.get_mut(id.0 as usize) {
            *fields = Rc::new(copies);
        }
    }

    /// Frees the string behind a handle and records the free.
    pub fn free(&mut self, id: StrId) {
        if let Some(slot) = self.slots.get_mut(id.0 as usize)
            && matches!(slot, Some(Object::Str(_)))
            && slot.take().is_some()
        {
            self.freed += 1;
            self.free_list.push(id.0);
        }
    }

    /// Frees the struct behind a handle, recursively dropping its fields.
    ///
    /// A struct cannot reach itself — the analyzer resolves field types in
    /// declaration order, so a cycle is unrepresentable — which is what bounds
    /// this recursion by the program's nesting depth.
    pub fn free_struct(&mut self, id: StructId) {
        let taken = match self.slots.get_mut(id.0 as usize) {
            Some(slot @ Some(Object::Struct(_))) => slot.take(),
            _ => None,
        };
        let Some(Object::Struct(fields)) = taken else {
            return;
        };
        self.freed += 1;
        self.free_list.push(id.0);
        // Only the last holder of the block owns what is in it. Another handle
        // still reading these fields would find them freed underneath it.
        let Ok(fields) = Rc::try_unwrap(fields) else {
            return;
        };
        for field in fields {
            self.drop_value(field);
        }
    }

    /// Current allocation counters.
    pub fn stats(&self) -> HeapStats {
        HeapStats {
            allocated: self.allocated,
            freed: self.freed,
            current: self.allocated - self.freed,
        }
    }

    /// Drops a value, freeing whatever heap storage it owns.
    pub fn drop_value(&mut self, value: Value) {
        match value {
            Value::Str(id) => self.free(id),
            Value::Struct(id) => self.free_struct(id),
            Value::Array(id) => self.free_array(id),
            Value::Enum(id) => self.free_enum(id),
            Value::Erased(id) => self.free_erased(id),
            Value::Cell(id) => self.free_cell(id),
            Value::NativeSnapshot(id) => self.free_snapshot(id),
            _ => {}
        }
    }

    /// Produces an independent copy of a value.
    ///
    /// Deep as far as any reader can tell: a struct's copy owns fresh copies of
    /// its fields, and an array's copy owns its elements from the first write
    /// through either handle. That is what makes `var b = a; b.x = 1` leave `a`
    /// alone, and the drop accounting stays provable — every *slot* has exactly
    /// one owner, so `current == 0` at exit still means the program balanced
    /// rather than that two owners freed one object once.
    ///
    /// **An array field inside a struct is independent too**, which falls out
    /// of the recursion rather than being a special case: the struct arm copies
    /// each field, and a field that is an array takes the array arm.
    ///
    /// Deep *by the time anyone can tell*: an array's copy shares its elements
    /// and takes them over on the first write through either handle, which no
    /// reader can distinguish from copying them here. See the module header.
    pub fn copy_value(&mut self, value: Value) -> Value {
        match value {
            Value::Str(id) => {
                let cloned = self.get(id).to_owned();
                Value::Str(self.alloc(cloned))
            }
            // The fields are shared rather than copied, on the array's terms
            // below: a fresh handle onto the same block, and the first writer
            // through either handle buys fields of its own
            // (`make_struct_unique`).
            Value::Struct(id) => {
                let shared = match self.slots.get(id.0 as usize) {
                    Some(Some(Object::Struct(fields))) => Rc::clone(fields),
                    _ => Rc::new(Vec::new()),
                };
                Value::Struct(StructId(self.alloc_object(Object::Struct(shared))))
            }
            // The elements are shared rather than copied: a fresh handle onto
            // the same ones, and whichever array is written first copies them
            // then (`make_array_unique`). Reading an array is most of what an
            // interpreted frame does, and doing this eagerly made it quadratic.
            Value::Array(id) => {
                let shared = match self.slots.get(id.0 as usize) {
                    Some(Some(Object::Array(elements))) => Rc::clone(elements),
                    // A handle that names no array copies to an empty one, the
                    // same answer `elements` gives a reader of one.
                    _ => Rc::new(Vec::new()),
                };
                Value::Array(ArrayId(self.alloc_object(Object::Array(shared))))
            }
            // A hold on the same object, not a new one. An enum is never
            // written through — a variant is replaced whole, and reading a
            // payload copies it out — so there is nothing a second holder could
            // observe, and no make-unique to do.
            Value::Enum(id) => {
                if let Some(Some(Object::Enum { shares, .. })) = self.slots.get_mut(id.0 as usize) {
                    *shares += 1;
                }
                Value::Enum(id)
            }
            // A hold on the same object, for the same reason an enum takes one:
            // nothing may write through an `Any`, so a second holder can
            // observe nothing a deep copy would have hidden.
            Value::Erased(id) => {
                if let Some(Some(Object::Erased { shares, .. })) = self.slots.get_mut(id.0 as usize)
                {
                    *shares += 1;
                }
                Value::Erased(id)
            }
            // A hold on the same box, and here the sharing is *observable* —
            // which is the point. A closure and the frame that declared the
            // `var` must see each other's writes, so a copy of a cell must not
            // be independent. This is the one arm of this function that does
            // not preserve value semantics, because the type it copies does not
            // have them.
            Value::Cell(id) => {
                if let Some(Some(Object::Cell { shares, .. })) = self.slots.get_mut(id.0 as usize) {
                    *shares += 1;
                }
                Value::Cell(id)
            }
            // A hold on the same node, for the same reason an enum takes one:
            // a snapshot is never written through, so a second holder can
            // observe nothing a deep copy would have hidden. This is the arm
            // that makes passing a state read down a recursion free.
            Value::NativeSnapshot(id) => {
                if let Some(Some(Object::Snapshot { shares, .. })) =
                    self.slots.get_mut(id.0 as usize)
                {
                    *shares += 1;
                }
                Value::NativeSnapshot(id)
            }
            scalar => scalar,
        }
    }
}
