//! Enums and erased values: two objects that carry a payload behind a tag.
//!
//! Together because they free the same way — a payload is released only when the
//! tag says one is there, and neither may drop a value another handle still
//! shares.

use super::*;

impl Heap {
    /// Allocates an enum value on the heap, returning its handle.
    ///
    /// The payload, when present, is taken rather than copied: whatever
    /// produced it (the operand stack) hands over ownership, exactly as a
    /// struct's fields are handed over.
    pub fn alloc_enum(&mut self, tag: u64, payload: Option<Value>) -> EnumId {
        let payload = payload.map(|payload| self.own(payload));
        EnumId(self.alloc_object(Object::Enum {
            tag,
            payload,
            shares: 1,
        }))
    }

    /// The discriminant tag of the enum behind a handle, or `None` when the
    /// handle does not name one.
    pub fn enum_tag(&self, id: EnumId) -> Option<u64> {
        match self.slots.get(id.0 as usize) {
            Some(Some(Object::Enum { tag, .. })) => Some(*tag),
            _ => None,
        }
    }

    /// An owned copy of the payload of the enum behind a handle.
    ///
    /// `None` when the handle does not name an enum, or when the variant it
    /// holds carries no payload. The copy is deep, exactly as
    /// [`Heap::copy_value`] is: the caller owns what it gets back and the box
    /// keeps owning its own, so freeing either leaves the other valid.
    pub fn enum_payload(&mut self, id: EnumId) -> Option<Value> {
        let payload = match self.slots.get(id.0 as usize) {
            Some(Some(Object::Enum { payload, .. })) => (*payload)?,
            _ => return None,
        };
        Some(self.copy_value(payload))
    }

    /// How many values hold the enum in slot `index`, or `None` when the slot
    /// does not hold one.
    pub(super) fn enum_shares(&self, index: u32) -> Option<u32> {
        match self.slots.get(index as usize) {
            Some(Some(Object::Enum { shares, .. })) => Some(*shares),
            _ => None,
        }
    }

    /// Releases one hold on an enum, dropping its payload with the last.
    ///
    /// Bounded by the program's nesting depth: a payload is a value analysis
    /// resolved against types that already resolve, so a cycle is
    /// unrepresentable — the same reason [`Heap::free_struct`] terminates.
    pub fn free_enum(&mut self, id: EnumId) {
        // Another value still reads this object, so nothing it owns goes here.
        if let Some(Some(Object::Enum { shares, .. })) = self.slots.get_mut(id.0 as usize)
            && *shares > 1
        {
            *shares -= 1;
            return;
        }
        let taken = match self.slots.get_mut(id.0 as usize) {
            Some(slot @ Some(Object::Enum { .. })) => slot.take(),
            _ => None,
        };
        let Some(Object::Enum { payload, .. }) = taken else {
            return;
        };
        self.freed += 1;
        self.free_list.push(id.0);
        if let Some(payload) = payload {
            self.drop_value(payload);
        }
    }

    /// Boxes `payload` as an erased value of type `type_id`.
    ///
    /// The payload is taken rather than copied: the operand stack hands over
    /// ownership, exactly as it does for an enum's payload.
    pub fn alloc_erased(&mut self, type_id: u64, payload: Value) -> ErasedId {
        let payload = self.own(payload);
        ErasedId(self.alloc_object(Object::Erased {
            type_id,
            payload,
            shares: 1,
        }))
    }

    /// The type id of the erased value behind a handle, or `None` when the
    /// handle does not name one.
    pub fn erased_type_id(&self, id: ErasedId) -> Option<u64> {
        match self.slots.get(id.0 as usize) {
            Some(Some(Object::Erased { type_id, .. })) => Some(*type_id),
            _ => None,
        }
    }

    /// The value behind an erasure, without copying it.
    ///
    /// A reader that only compares wants neither a copy nor a `&mut`, and a
    /// [`Value`] is `Copy` — the same arrangement [`Heap::enum_payload_ref`]
    /// uses, and for the same reason.
    pub(super) fn erased_payload_ref(&self, id: ErasedId) -> Option<Value> {
        match self.slots.get(id.0 as usize) {
            Some(Some(Object::Erased { payload, .. })) => Some(*payload),
            _ => None,
        }
    }

    /// Releases one hold on an erasure, dropping what it holds with the last.
    ///
    /// Bounded by the value's nesting depth, exactly as [`Heap::free_enum`] is.
    pub fn free_erased(&mut self, id: ErasedId) {
        // Another value still reads this object, so nothing it owns goes here.
        if let Some(Some(Object::Erased { shares, .. })) = self.slots.get_mut(id.0 as usize)
            && *shares > 1
        {
            *shares -= 1;
            return;
        }
        let taken = match self.slots.get_mut(id.0 as usize) {
            Some(slot @ Some(Object::Erased { .. })) => slot.take(),
            _ => None,
        };
        let Some(Object::Erased { payload, .. }) = taken else {
            return;
        };
        self.freed += 1;
        self.free_list.push(id.0);
        self.drop_value(payload);
    }
}
