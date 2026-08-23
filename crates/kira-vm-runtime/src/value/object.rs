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
        // Validated before ownership is taken. `own` allocates heap objects and
        // consumes the caller's `NativeSnapshot` slot, so returning `false`
        // after it would leak what it allocated *and* hand the caller back a
        // handle it believes it still owns — the doc above promises this call
        // "changed nothing and dropped nothing" when it refuses.
        if self.field(id, index).is_none() {
            return false;
        }
        let value = self.own(value);
        // Fields of this struct's own before anything lands in them: a write
        // through one handle must not be visible through another that copied it.
        self.make_struct_unique(id);
        // Read *after* unsharing. The handle in the shared block belongs to
        // every holder of it; dropping that one would free the storage the other
        // holders are still reading.
        let Some(previous) = self.field(id, index) else {
            // Unreachable given the check above, but if unsharing ever left the
            // field unreadable the owned value would be ours to release: it is
            // no longer the caller's and nothing else will free it.
            self.drop_value(value);
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

    /// Copies `text` into a heap-owned, NUL-terminated C block, or `None` when
    /// an interior NUL means the bytes C would read are not the bytes Kira
    /// holds.
    pub fn cblock_text(&mut self, text: &str) -> Option<CBlockId> {
        let bytes = kira_runtime_abi::c_storage::nul_terminated(text)?;
        Some(self.cblock_bytes(bytes))
    }

    /// Moves `bytes` into a heap-owned C block.
    pub fn cblock_bytes(&mut self, bytes: Vec<u8>) -> CBlockId {
        CBlockId(self.alloc_object(Object::CBlock {
            bytes: bytes.into_boxed_slice(),
            children: Vec::new(),
        }))
    }

    /// Moves `child` under `parent` and patches the embedded pointer word.
    pub fn cblock_attach(
        &mut self,
        parent: CBlockId,
        offset: kira_runtime_abi::CBlockOffset,
        width: kira_runtime_abi::ForeignPointerWidth,
        child: CBlockId,
    ) -> bool {
        let word = self.cblock_address(child).to_le_bytes();
        let start = match usize::try_from(offset.bytes()) {
            Ok(start) => start,
            Err(_) => return false,
        };
        let width_bytes = width.bytes() as usize;
        let Some(end) = start.checked_add(width_bytes) else {
            return false;
        };
        let Some(Some(Object::CBlock { bytes, children })) = self.slots.get_mut(parent.0 as usize)
        else {
            return false;
        };
        let Some(target) = bytes.get_mut(start..end) else {
            return false;
        };
        target.copy_from_slice(&word[..width_bytes]);
        children.push(VmCBlockChild {
            offset,
            width,
            block: child,
        });
        true
    }

    /// The address a foreign callee reads this block's bytes at.
    ///
    /// Zero for a freed or non-block handle — the same null a refused C string
    /// crosses as — so a misbehaving caller hands C a null rather than a
    /// dangling pointer.
    pub fn cblock_address(&self, id: CBlockId) -> u64 {
        match self.slots.get(id.0 as usize) {
            Some(Some(Object::CBlock { bytes, .. })) => bytes.as_ptr() as usize as u64,
            _ => 0,
        }
    }

    /// Frees the C block behind a handle, drops whatever it kept alive, and
    /// records the free.
    pub fn free_cblock(&mut self, id: CBlockId) {
        let taken = match self.slots.get_mut(id.0 as usize) {
            Some(slot @ Some(Object::CBlock { .. })) => slot.take(),
            _ => None,
        };
        let Some(Object::CBlock { children, .. }) = taken else {
            return;
        };
        self.freed += 1;
        self.free_list.push(id.0);
        for child in children {
            self.free_cblock(child.block);
        }
    }

    /// Transfers `value` to the retained registry: alive, slots occupied,
    /// until the heap itself drops.
    ///
    /// This is what a `retains:` foreign parameter does to its argument — the
    /// callee kept pointers into the value's C blocks, so freeing any of it on
    /// a schedule this side can guess would hand C dangling storage. Teardown
    /// is the one moment provably after every foreign call has returned.
    pub fn retain_for_foreign(&mut self, value: Value) {
        self.retained.push(value);
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
            retained: self.retained.len() as u64,
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
            Value::CBlock(id) => self.free_cblock(id),
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
            // A genuinely deep copy, and deliberately the only kind here that
            // takes one: a C block has exactly one owner, so a second holder
            // needs bytes of its own at a fresh address. The ownership
            // checker's move rules keep this arm off every hot path — a copy
            // happens only where a struct unshares, never per read.
            //
            // The kept value copies with it — but the fresh image's pointer
            // members still name the *original*'s storage, which the fresh
            // hold keeps alive exactly as the original block did.
            Value::CBlock(id) => Value::CBlock(self.copy_cblock(id)),
            scalar => scalar,
        }
    }

    /// Deep-clones one uniquely owned C-block tree.
    fn copy_cblock(&mut self, id: CBlockId) -> CBlockId {
        let (bytes, children) = match self.slots.get(id.0 as usize) {
            Some(Some(Object::CBlock { bytes, children })) => (bytes.to_vec(), children.clone()),
            _ => (Vec::new(), Vec::new()),
        };
        let copy = self.cblock_bytes(bytes);
        for child in children {
            let child_copy = self.copy_cblock(child.block);
            let _ = self.cblock_attach(copy, child.offset, child.width, child_copy);
        }
        copy
    }
}

#[cfg(test)]
mod tests {
    use kira_runtime_abi::{CBlockOffset, ForeignPointerWidth};

    use super::*;

    #[test]
    fn a_cblock_tree_clone_rewrites_its_embedded_pointer() {
        let mut heap = Heap::new();
        let child = heap.cblock_bytes(vec![7, 8, 9]);
        let root = heap.cblock_bytes(vec![0; 8]);
        assert!(heap.cblock_attach(
            root,
            CBlockOffset::new(0),
            ForeignPointerWidth::Bits64,
            child,
        ));
        let Value::CBlock(copy) = heap.copy_value(Value::CBlock(root)) else {
            panic!("a C-block copy stays a C block");
        };
        // SAFETY: `root` owns an eight-byte live image payload.
        let root_word =
            unsafe { kira_runtime_abi::c_storage::read_bytes(heap.cblock_address(root), 0, 8) }
                .map(u64::from_le_bytes)
                .expect("the original image is readable");
        // SAFETY: `copy` owns an eight-byte live image payload.
        let copy_word =
            unsafe { kira_runtime_abi::c_storage::read_bytes(heap.cblock_address(copy), 0, 8) }
                .map(u64::from_le_bytes)
                .expect("the copied image is readable");
        assert_eq!(root_word, heap.cblock_address(child));
        assert_ne!(copy_word, root_word);
        // SAFETY: `copy_word` is the payload address of the cloned child.
        let bytes = unsafe { kira_runtime_abi::c_storage::read_bytes(copy_word, 0, 3) }
            .expect("the cloned child is readable");
        assert_eq!(&bytes[..3], &[7, 8, 9]);
        heap.drop_value(Value::CBlock(root));
        heap.drop_value(Value::CBlock(copy));
        assert_eq!(heap.stats().current, 0);
    }
}
