//! Conversion between VM heap values and backend-neutral callback state.

use kira_runtime_abi::NativeStateValue;

use super::{Heap, Object, Value};

impl Heap {
    /// Moves a runtime value into the backend-neutral callback-state tree.
    ///
    /// The error names the shape that had no boxed form, as a noun phrase a
    /// message reads as a subject: a refusal deep inside a struct is otherwise
    /// indistinguishable from a refusal of the struct itself.
    pub fn into_native_state(&mut self, value: Value) -> Result<NativeStateValue, &'static str> {
        Ok(match value {
            Value::Int(value) => NativeStateValue::Int(value),
            Value::Float(value) => NativeStateValue::Float(value),
            Value::Bool(value) => NativeStateValue::Bool(value),
            Value::RawPtr(value) => NativeStateValue::RawPtr(value),
            Value::Str(id) => match self.take_object(id.0) {
                Some(Object::Str(value)) => NativeStateValue::String(value),
                _ => return Err("a string whose storage was already taken"),
            },
            Value::Struct(id) => {
                // Moving the fields out is a write like any other: a struct
                // sharing them would be left reading values this took over.
                self.make_struct_unique(id);
                let fields = match self.take_object(id.0) {
                    Some(Object::Struct(fields)) => fields,
                    _ => return Err("a struct whose fields were already taken"),
                };
                // Sole owner by now, and the slot just gave up its hold.
                let Ok(fields) = std::rc::Rc::try_unwrap(fields) else {
                    return Err("a struct still shared with another value");
                };
                let values = self.native_state_children(fields)?;
                NativeStateValue::struct_of(values)
            }
            Value::Array(id) => {
                // Moving the elements out is a write like any other: an array
                // sharing them would be left reading values this took over.
                self.make_array_unique(id);
                let elements = match self.take_object(id.0) {
                    Some(Object::Array(elements)) => elements,
                    _ => return Err("an array whose elements were already taken"),
                };
                // Sole owner by now, and the slot just gave up its hold.
                let Ok(elements) = std::rc::Rc::try_unwrap(elements) else {
                    return Err("an array still shared with another value");
                };
                let values = self.native_state_children(elements)?;
                NativeStateValue::array_of(values)
            }
            // Moving the payload out is the one thing an enum is never asked to
            // do elsewhere, and a shared object cannot answer it: the values
            // still holding it would be left reading what this took. They get
            // the object; this gets a copy of the payload.
            Value::Enum(id) => {
                // The tag is checked before anything is consumed: taking the
                // object first would strand its payload when the tag turns out
                // not to fit callback state.
                let tag = match self.slots.get(id.0 as usize) {
                    Some(Some(Object::Enum { tag, .. })) => *tag,
                    _ => return Err("an enum whose payload was already taken"),
                };
                let tag =
                    u32::try_from(tag).map_err(|_| "an enum tag too large for callback state")?;
                let payload = match self.enum_shares(id.0) {
                    Some(1) | None => match self.take_object(id.0) {
                        Some(Object::Enum { payload, .. }) => payload,
                        _ => return Err("an enum whose payload was already taken"),
                    },
                    Some(_) => {
                        let payload = match self.slots.get(id.0 as usize) {
                            Some(Some(Object::Enum { payload, .. })) => *payload,
                            _ => return Err("an enum whose payload was already taken"),
                        };
                        let payload = payload.map(|value| self.copy_value(value));
                        self.free_enum(super::EnumId(id.0));
                        payload
                    }
                };
                NativeStateValue::enum_of(
                    tag,
                    match payload {
                        Some(value) => Some(self.into_native_state(value)?),
                        None => None,
                    },
                )
            }
            Value::Erased(id) => {
                let (type_id, payload, shares) = match self.slots.get(id.0 as usize) {
                    Some(Some(Object::Erased {
                        type_id,
                        payload,
                        shares,
                    })) => (*type_id, *payload, *shares),
                    _ => return Err("an erased value whose payload was already taken"),
                };
                let payload = if shares == 1 {
                    match self.take_object(id.0) {
                        Some(Object::Erased { payload, .. }) => payload,
                        _ => return Err("an erased value whose payload was already taken"),
                    }
                } else {
                    let payload = self.copy_value(payload);
                    self.free_erased(id);
                    payload
                };
                let payload = self.into_native_state(payload)?;
                NativeStateValue::any_of(type_id, payload)
            }
            // A read of callback state going back into callback state: the node
            // it holds is already in the stored form, so this is where the
            // deferral pays off most — `state.a = state.b` moves a node instead
            // of rebuilding a subtree as objects and encoding it again.
            Value::NativeSnapshot(id) => {
                let Some(node) = self.snapshot_node(id).cloned() else {
                    return Err("a callback-state read whose node was already taken");
                };
                self.free_snapshot(id);
                node
            }
            // The one value that goes in *shared* rather than copied. A closure
            // inside the state and the frame that declared the `var` are two
            // holders of one box, and boxing a copy of what it holds would give
            // them a box each. So the tree takes over this value's hold and
            // gives it back when its last node goes.
            Value::Cell(id) => NativeStateValue::Cell(self.cell_into_native_state(id)),
            // The block's bytes move with the node; the engine that absorbs
            // the tree materializes a block it owns, so the pointer C reads is
            // always inside the engine holding the value.
            Value::CBlock(id) => NativeStateValue::CBlock(self.take_cblock_tree(id)?),
            Value::Void => return Err("a void value"),
            Value::NativeState(_) => return Err("callback state inside callback state"),
            Value::NativeView { .. } => {
                return Err("a recovered callback-state view inside callback state");
            }
        })
    }

    /// Builds a heap value from a backend-neutral callback-state tree.
    ///
    /// Reads the tree rather than consuming it. The stored node is shared —
    /// every aggregate holds its children behind an `Arc` — so a caller that had
    /// to hand over ownership would have to clone the whole subtree first, and
    /// then this would walk the clone and free it again. Two walks and a full
    /// copy to build what one walk builds.
    pub fn from_native_state(&mut self, value: &NativeStateValue) -> Value {
        match value {
            NativeStateValue::Int(value) => Value::Int(*value),
            NativeStateValue::Float(value) => Value::Float(*value),
            NativeStateValue::Bool(value) => Value::Bool(*value),
            NativeStateValue::RawPtr(value) => Value::RawPtr(*value),
            NativeStateValue::String(value) => Value::Str(self.alloc(value.clone())),
            NativeStateValue::Struct(fields) => {
                let fields = fields
                    .iter()
                    .map(|field| self.from_native_state(field))
                    .collect();
                Value::Struct(self.alloc_struct(fields))
            }
            NativeStateValue::Array(elements) => {
                let elements = elements
                    .iter()
                    .map(|element| self.from_native_state(element))
                    .collect();
                Value::Array(self.alloc_array(elements))
            }
            NativeStateValue::Enum { tag, payload } => {
                let payload = payload
                    .as_deref()
                    .map(|value| self.from_native_state(value));
                Value::Enum(self.alloc_enum(u64::from(*tag), payload))
            }
            NativeStateValue::Any { type_id, payload } => {
                let payload = self.from_native_state(payload);
                Value::Erased(self.alloc_erased(*type_id, payload))
            }
            // Reading a cell out of state gives back the *same* box, not a copy
            // of one: that is what makes a write through the recovered value and
            // a write through the frame's own binding land in one place. The
            // tree keeps its own hold, so this is a fresh one.
            //
            // Only a VM-owned handle names a slot in this heap; a native
            // engine's handle is its own opaque word, so it crosses back as the
            // pointer it is rather than being narrowed into a garbage slot id.
            NativeStateValue::Cell(cell)
                if cell.is_vm_owned() && cell.handle() <= u32::MAX as u64 =>
            {
                self.copy_value(Value::Cell(super::CellId(cell.handle() as u32)))
            }
            NativeStateValue::Cell(cell) => Value::RawPtr(cell.handle()),
            // The bytes come back as a block this heap owns; the node keeps
            // its own copy, exactly as a string's text does above.
            NativeStateValue::CBlock(block) => Value::CBlock(self.copy_native_cblock(block)),
        }
    }

    fn native_state_children(
        &mut self,
        children: Vec<Value>,
    ) -> Result<Vec<NativeStateValue>, &'static str> {
        let mut remaining = children.into_iter();
        let mut converted = Vec::with_capacity(remaining.len());
        while let Some(child) = remaining.next() {
            match self.into_native_state(child) {
                Ok(value) => converted.push(value),
                Err(error) => {
                    for child in remaining {
                        self.drop_value(child);
                    }
                    return Err(error);
                }
            }
        }
        Ok(converted)
    }

    /// Moves one VM C-block tree into its backend-neutral representation.
    fn take_cblock_tree(
        &mut self,
        id: super::CBlockId,
    ) -> Result<kira_runtime_abi::NativeCBlock, &'static str> {
        let Some(Object::CBlock { bytes, children }) = self.take_object(id.0) else {
            return Err("a C block whose storage was already taken");
        };
        let mut block = kira_runtime_abi::NativeCBlock::new(bytes.into_vec());
        for child in children {
            let nested = self.take_cblock_tree(child.block)?;
            block
                .attach(child.offset, child.width, nested)
                .map_err(|_| "a C block with a child outside its payload")?;
        }
        Ok(block)
    }

    /// Copies one backend-neutral C-block tree into this heap.
    fn copy_native_cblock(&mut self, block: &kira_runtime_abi::NativeCBlock) -> super::CBlockId {
        let root = self.cblock_bytes(block.bytes().to_vec());
        for child in block.children() {
            let nested = self.copy_native_cblock(child.block());
            let _ = self.cblock_attach(root, child.offset(), child.width(), nested);
        }
        root
    }

    fn take_object(&mut self, index: u32) -> Option<Object> {
        let object = self.slots.get_mut(index as usize)?.take()?;
        self.freed += 1;
        self.free_list.push(index);
        Some(object)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refused_child_releases_the_rest_of_a_recursive_state_value() {
        let mut heap = Heap::new();
        let nested_text = heap.alloc("nested".to_owned());
        let trailing_text = heap.alloc("trailing".to_owned());
        let nested = heap.alloc_struct(vec![Value::Str(nested_text), Value::Void]);
        let root = heap.alloc_struct(vec![Value::Struct(nested), Value::Str(trailing_text)]);

        let error = heap
            .into_native_state(Value::Struct(root))
            .expect_err("void is refused inside callback state");

        assert_eq!(error, "a void value");
        assert_eq!(heap.stats().current, 0);
    }
}
