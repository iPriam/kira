//! Runtime values and the object heap with affine drop accounting.
//!
//! Scalars (`Int`, `Float`, `Bool`, `Void`) are `Copy` and live inline in a
//! [`Value`]. Strings and structs live on a [`Heap`]; a [`Value::Str`] or
//! [`Value::Struct`] is a handle into it. Both follow value semantics with
//! affine drops: reading a local *copies* it (a fresh allocation, deep for a
//! struct) and every instruction that consumes one *frees* it, so a well-formed
//! run ends with [`HeapStats::current`] at zero.
//!
//! A struct is a plain tuple of values: the VM is structurally typed, so it
//! never learns a struct's name or its field names. The compiler resolved those
//! to indices, which is what lets the same heap serve both kinds of object.

use kira_runtime_abi::{NativeStateToken, NativeStateTypeId};

/// A handle to a heap-allocated string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StrId(u32);

/// A handle to a heap-allocated struct value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StructId(u32);

/// A handle to a heap-allocated array value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArrayId(u32);

/// A handle to a heap-allocated enum value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnumId(u32);

/// A runtime value on the operand stack or in a local slot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Value {
    /// A 64-bit signed integer.
    Int(i64),
    /// A 64-bit float.
    Float(f64),
    /// A boolean.
    Bool(bool),
    /// A handle to a heap string.
    Str(StrId),
    /// A handle to a heap struct.
    Struct(StructId),
    /// A handle to a heap array.
    Array(ArrayId),
    /// A handle to a heap enum value.
    Enum(EnumId),
    /// An opaque, target-width pointer word from a foreign (`@FFI.Extern`) call.
    ///
    /// Inline and `Copy` like the other scalars: it owns no heap storage, and
    /// the VM never dereferences, does arithmetic on, or frees it. It only ever
    /// arrives from and returns to the foreign seam
    /// ([`kira_runtime_abi::HostCapabilities::call_foreign`]).
    RawPtr(u64),
    /// An opaque owning handle to native callback state.
    NativeState(NativeStateToken),
    /// A typed mutable view through an opaque callback-state token.
    NativeView {
        /// The stable userdata token.
        token: NativeStateToken,
        /// The type identity recovery validated.
        type_id: NativeStateTypeId,
    },
    /// The unit value.
    Void,
}

/// What a heap slot holds.
#[derive(Debug, Clone, PartialEq)]
enum Object {
    /// A string's bytes.
    Str(String),
    /// A struct's fields, in declaration order.
    Struct(Vec<Value>),
    /// An array's elements, in order.
    Array(Vec<Value>),
    /// An enum value: a discriminant tag and its optional single payload.
    Enum {
        /// The variant's declaration index.
        tag: u32,
        /// The payload value, absent for a payload-less variant.
        payload: Option<Value>,
    },
}

/// A snapshot of heap allocation counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeapStats {
    /// Total string allocations performed over the run.
    pub allocated: u64,
    /// Total string frees performed over the run.
    pub freed: u64,
    /// Live strings right now (`allocated - freed`).
    pub current: u64,
}

/// The object heap: owns every live string and struct, and counts allocations
/// and frees.
///
/// Strings and structs share one slot table and one pair of counters, so
/// `current == 0` at exit proves *both* kinds balanced rather than only one.
#[derive(Debug, Default)]
pub struct Heap {
    slots: Vec<Option<Object>>,
    free_list: Vec<u32>,
    allocated: u64,
    freed: u64,
}

impl Heap {
    /// Creates an empty heap.
    pub fn new() -> Self {
        Self::default()
    }

    /// Allocates `value` on the heap, returning its handle.
    pub fn alloc(&mut self, value: String) -> StrId {
        StrId(self.alloc_object(Object::Str(value)))
    }

    /// Allocates a struct of `fields` on the heap, returning its handle.
    ///
    /// The fields are taken, not copied: whatever produced them (the operand
    /// stack) hands over ownership, exactly as a `Str` handle is handed over.
    pub fn alloc_struct(&mut self, fields: Vec<Value>) -> StructId {
        StructId(self.alloc_object(Object::Struct(fields)))
    }

    /// Allocates an array of `elements` on the heap, returning its handle.
    ///
    /// As with a struct, the elements are taken rather than copied: whatever
    /// produced them hands over ownership.
    pub fn alloc_array(&mut self, elements: Vec<Value>) -> ArrayId {
        ArrayId(self.alloc_object(Object::Array(elements)))
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
        let Some(previous) = self.element(id, index) else {
            return false;
        };
        self.drop_value(previous);
        match self.slots.get_mut(id.0 as usize) {
            Some(Some(Object::Array(elements))) => match elements.get_mut(index) {
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
        match self.slots.get_mut(id.0 as usize) {
            Some(Some(Object::Array(elements))) => {
                elements.push(value);
                true
            }
            _ => false,
        }
    }

    /// Frees the array behind a handle, recursively dropping its elements.
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
        for element in elements {
            self.drop_value(element);
        }
    }

    /// Allocates an enum value on the heap, returning its handle.
    ///
    /// The payload, when present, is taken rather than copied: whatever
    /// produced it (the operand stack) hands over ownership, exactly as a
    /// struct's fields are handed over.
    pub fn alloc_enum(&mut self, tag: u32, payload: Option<Value>) -> EnumId {
        EnumId(self.alloc_object(Object::Enum { tag, payload }))
    }

    /// The discriminant tag of the enum behind a handle, or `None` when the
    /// handle does not name one.
    pub fn enum_tag(&self, id: EnumId) -> Option<u32> {
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

    /// Frees the enum behind a handle, dropping its payload.
    ///
    /// Bounded by the program's nesting depth: a payload is a value analysis
    /// resolved against types that already resolve, so a cycle is
    /// unrepresentable — the same reason [`Heap::free_struct`] terminates.
    pub fn free_enum(&mut self, id: EnumId) {
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

    fn alloc_object(&mut self, object: Object) -> u32 {
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
    pub fn field(&self, id: StructId, index: u16) -> Option<Value> {
        self.fields(id).get(index as usize).copied()
    }

    /// Replaces field `index` of a struct, dropping what was there, and
    /// reports whether the field existed.
    ///
    /// Returns `false` — having changed nothing and dropped nothing — when the
    /// handle does not name a struct with that field.
    pub fn set_field(&mut self, id: StructId, index: u16, value: Value) -> bool {
        let Some(previous) = self.field(id, index) else {
            return false;
        };
        self.drop_value(previous);
        match self.slots.get_mut(id.0 as usize) {
            Some(Some(Object::Struct(fields))) => match fields.get_mut(index as usize) {
                Some(slot) => {
                    *slot = value;
                    true
                }
                None => false,
            },
            _ => false,
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
            _ => {}
        }
    }

    /// Produces an independent copy of a value.
    ///
    /// Deep by construction: a struct's copy owns fresh copies of its fields
    /// and an array's copy owns fresh copies of its elements, so no two live
    /// values ever share heap storage. That is what makes `var b = a; b.x = 1`
    /// leave `a` alone, and it is what keeps the drop accounting provable —
    /// every handle has exactly one owner, so `current == 0` at exit means the
    /// program balanced rather than that two owners freed one object once.
    ///
    /// **An array field inside a struct is deep-copied too**, which falls out
    /// of the recursion rather than being a special case: the struct arm copies
    /// each field, and a field that is an array takes the array arm.
    ///
    /// This is the one place the copy is expensive: reading an array copies all
    /// of it, so `xs[i]` inside a loop is quadratic. That is the existing cost
    /// model — a struct field read already deep-copies its struct — and the fix
    /// is the by-reference load the `borrow mut` work needs, not a special case
    /// here. See `.codex/work/arrays.md`.
    pub fn copy_value(&mut self, value: Value) -> Value {
        match value {
            Value::Str(id) => {
                let cloned = self.get(id).to_owned();
                Value::Str(self.alloc(cloned))
            }
            Value::Struct(id) => {
                let fields = self.fields(id).to_vec();
                let copies = fields
                    .into_iter()
                    .map(|field| self.copy_value(field))
                    .collect();
                Value::Struct(self.alloc_struct(copies))
            }
            Value::Array(id) => {
                let elements = self.elements(id).to_vec();
                let copies = elements
                    .into_iter()
                    .map(|element| self.copy_value(element))
                    .collect();
                Value::Array(self.alloc_array(copies))
            }
            Value::Enum(id) => {
                // Deep, like a struct or an array: the copy owns a fresh box
                // and a fresh copy of the payload, so no two live enums share a
                // handle and neither drop frees the other's.
                let (tag, payload) = match self.slots.get(id.0 as usize) {
                    Some(Some(Object::Enum { tag, payload })) => (*tag, *payload),
                    _ => (0, None),
                };
                let payload = payload.map(|value| self.copy_value(value));
                Value::Enum(self.alloc_enum(tag, payload))
            }
            scalar => scalar,
        }
    }
}

mod native_state;
mod seam;

#[cfg(test)]
#[path = "value_tests.rs"]
mod tests;
