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
//!
//! # Arrays copy when they are written, not when they are read
//!
//! An array's elements are the one thing here that is *shared*: copying an
//! array takes a new slot pointing at the same elements, and a write through
//! either one gives the writer elements of its own first
//! ([`Heap::make_array_unique`]). Nothing observable changes — the two arrays
//! behave exactly as two deep copies — but reading an array stops costing the
//! whole array, which is what an interpreted UI frame is mostly made of. The
//! native runtime shares an array's item block on the same terms; this is one
//! design serving both engines rather than two.

use std::rc::Rc;

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

/// A handle to a heap-allocated capture cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellId(u32);

/// A handle to a heap-allocated erased (`Any`) value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ErasedId(u32);

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
    /// A handle to an erased value: what a value becomes on crossing into
    /// `Any`, carrying the type it had on the way in.
    Erased(ErasedId),
    /// A handle to a capture cell: the shared, mutable storage a `var` moves
    /// into when a closure captures it.
    ///
    /// The one value here with reference semantics. Copying one takes another
    /// hold on the same box — deliberately, because a closure and the frame
    /// that wrote the `var` have to see each other's writes — and the last hold
    /// releases what is inside. Everything else on this heap is a value, and
    /// a cell being the exception is the whole of the feature.
    Cell(CellId),
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
    /// An array's elements, in order, shared until one holder writes.
    ///
    /// The one shared object on this heap. Every other kind is exclusively
    /// owned, which is what lets a place walk move handles; an array earns the
    /// exception by being the expensive one to copy and the common one to read.
    Array(Rc<Vec<Value>>),
    /// An enum value: a discriminant tag and its optional single payload,
    /// shared by every value that copied it.
    ///
    /// Shared for the same reason an array's elements are, and more simply: an
    /// enum object is never written through — a variant is replaced whole, and
    /// every read of one hands back an owned copy — so a copy needs no object
    /// of its own and there is nothing to make unique.
    Enum {
        /// The variant's declaration index.
        tag: u32,
        /// The payload value, absent for a payload-less variant.
        payload: Option<Value>,
        /// How many values hold this object; the payload goes with the last.
        shares: u32,
    },
    /// An erased value: the type it had before `Any` took it away, and the
    /// value itself.
    ///
    /// Shared and never written through, exactly as an enum object is, and for
    /// the same reason: nothing may write through an `Any`, so a copy needs no
    /// object of its own.
    ///
    /// The VM would not need a box here to *carry* an erased value — its
    /// values are tagged already, which is why erasure emitted no instruction
    /// before this existed. It needs one to *compare* them. A struct object on
    /// this heap is a tuple of values with no record of which declaration built
    /// it, so without the id written here `Point(1, 2)` and `Rect(1, 2)` would
    /// compare equal on the VM and could not compare at all on native. See
    /// [`kira_semantics_model::ErasedTypeId`].
    Erased {
        /// The [`kira_semantics_model::ErasedTypeId`] word of the type that
        /// crossed in.
        type_id: u64,
        /// The value that crossed in.
        payload: Value,
        /// How many values hold this object; the payload goes with the last.
        shares: u32,
    },
    /// A capture cell: one value, shared by every holder, and written through.
    ///
    /// The one *mutable* shared object on this heap. An enum is shared and
    /// never written through, and an array's elements are shared only until a
    /// writer buys its own; a cell is shared precisely so that a write through
    /// one holder is visible through the others.
    Cell {
        /// What the cell holds. Replaced whole by a write, never edited in
        /// place — see [`Heap::cell_set`].
        payload: Value,
        /// How many values hold this box; the payload goes with the last.
        shares: u32,
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
        ArrayId(self.alloc_object(Object::Array(Rc::new(elements))))
    }

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
        // Bounds first, so an index that is about to trap copies nothing; then
        // the copy, and only then the read of what is being replaced. In that
        // order the value dropped below is this array's own — read before the
        // copy it would be the one every sharer still holds.
        if !matches!(self.array_len(id), Some(len) if index < len) {
            return false;
        }
        self.make_array_unique(id);
        let Some(previous) = self.element(id, index) else {
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

    /// Allocates an enum value on the heap, returning its handle.
    ///
    /// The payload, when present, is taken rather than copied: whatever
    /// produced it (the operand stack) hands over ownership, exactly as a
    /// struct's fields are handed over.
    pub fn alloc_enum(&mut self, tag: u32, payload: Option<Value>) -> EnumId {
        EnumId(self.alloc_object(Object::Enum {
            tag,
            payload,
            shares: 1,
        }))
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

    /// How many values hold the enum in slot `index`, or `None` when the slot
    /// does not hold one.
    fn enum_shares(&self, index: u32) -> Option<u32> {
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
    fn erased_payload_ref(&self, id: ErasedId) -> Option<Value> {
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

    /// Boxes `payload` into a fresh capture cell, returning its handle.
    ///
    /// The payload is taken rather than copied: whatever produced it (the
    /// operand stack) hands over ownership, exactly as an enum's payload is
    /// handed over.
    pub fn alloc_cell(&mut self, payload: Value) -> CellId {
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
    pub fn cell_set(&mut self, id: CellId, value: Value) -> bool {
        let previous = match self.slots.get_mut(id.0 as usize) {
            Some(Some(Object::Cell { payload, .. })) => std::mem::replace(payload, value),
            _ => return false,
        };
        self.drop_value(previous);
        true
    }

    /// Releases one hold on a capture cell, dropping its payload with the last.
    ///
    /// Bounded by the program's nesting depth for every value a cell can hold
    /// *except* one: a cell holding a closure that captures the same cell is a
    /// genuine reference cycle, and share counts cannot collect it. That leaks
    /// the box and its payload — memory-safe, never freed twice, never freed
    /// early — and is recorded rather than defended against, because detecting
    /// it needs a tracing collector this runtime does not have.
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
            Value::Erased(id) => self.free_erased(id),
            Value::Cell(id) => self.free_cell(id),
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
            Value::Struct(id) => {
                let fields = self.fields(id).to_vec();
                let copies = fields
                    .into_iter()
                    .map(|field| self.copy_value(field))
                    .collect();
                Value::Struct(self.alloc_struct(copies))
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
            scalar => scalar,
        }
    }

    /// Whether two values are structurally equal.
    ///
    /// What `EqAny` answers. Handles are followed rather than compared: two
    /// strings are equal when their bytes are, two structs when every field
    /// pair is, two arrays when they have the same length and every element
    /// pair is, and two enums when their tags and payloads are. So a value and
    /// an independent copy of it compare equal, which is the whole point —
    /// nothing that reaches here can rely on having been the same object.
    ///
    /// Values of different kinds are unequal rather than an error. `EqAny` is
    /// the one comparison whose operands are not known to agree statically, and
    /// a caller asking whether an `Int` equals a `String` is asking a question
    /// with an answer.
    ///
    /// `Float` compares as `EqFloat` does, on the bit-level `==` of `f64`, so
    /// `NaN` is equal to nothing including itself. Making erasure the one place
    /// where `NaN` compares equal would be a worse surprise than the IEEE rule.
    ///
    /// Bounded by the value's nesting depth for the same reason
    /// [`Heap::free_struct`] is: a payload is a value analysis resolved against
    /// types that already resolve, so a cycle is unrepresentable.
    pub fn values_equal(&self, left: Value, right: Value) -> bool {
        match (left, right) {
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a == b,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::RawPtr(a), Value::RawPtr(b)) => a == b,
            (Value::Void, Value::Void) => true,
            (Value::Str(a), Value::Str(b)) => self.get(a) == self.get(b),
            (Value::Struct(a), Value::Struct(b)) => {
                let (left, right) = (self.fields(a), self.fields(b));
                left.len() == right.len()
                    && left
                        .iter()
                        .zip(right)
                        .all(|(&one, &other)| self.values_equal(one, other))
            }
            (Value::Array(a), Value::Array(b)) => {
                let (left, right) = (self.elements(a), self.elements(b));
                left.len() == right.len()
                    && left
                        .iter()
                        .zip(right)
                        .all(|(&one, &other)| self.values_equal(one, other))
            }
            // The arm `EqAny` actually reaches, and the only one that consults
            // a nominal identity. Once the two ids agree, both sides are known
            // to be the same Kira type, which is what makes the structural
            // walk below sound: a `Point`'s fields are never read as a
            // `Rect`'s. Ids differing is an ordinary `false`.
            (Value::Erased(a), Value::Erased(b)) => {
                self.erased_type_id(a) == self.erased_type_id(b)
                    && match (self.erased_payload_ref(a), self.erased_payload_ref(b)) {
                        (Some(one), Some(other)) => self.values_equal(one, other),
                        _ => false,
                    }
            }
            (Value::Enum(a), Value::Enum(b)) => {
                self.enum_tag(a) == self.enum_tag(b)
                    && match (self.enum_payload_ref(a), self.enum_payload_ref(b)) {
                        (Some(one), Some(other)) => self.values_equal(one, other),
                        (None, None) => true,
                        _ => false,
                    }
            }
            // A cell has reference semantics, so identity *is* its equality —
            // two cells with equal contents are still two places to write. It
            // cannot reach here through `EqAny` regardless: a cell does not
            // erase into `Any` (`Type::assignable_to`).
            (Value::Cell(a), Value::Cell(b)) => a == b,
            // Opaque handles into a host's storage. This runtime cannot read
            // what is behind one, so identity is the only honest answer, and
            // neither erases into `Any` either.
            (Value::NativeState(a), Value::NativeState(b)) => a == b,
            (
                Value::NativeView {
                    token: a,
                    type_id: a_ty,
                },
                Value::NativeView {
                    token: b,
                    type_id: b_ty,
                },
            ) => a == b && a_ty == b_ty,
            _ => false,
        }
    }

    /// The payload of the enum behind a handle, without copying it.
    ///
    /// [`Heap::enum_payload`] hands back an owned copy because its callers take
    /// the payload away from the box. A reader that only compares wants neither
    /// the copy nor the `&mut`, and a `Value` is `Copy`, so the handle comes
    /// back as-is and stays owned by the box.
    fn enum_payload_ref(&self, id: EnumId) -> Option<Value> {
        match self.slots.get(id.0 as usize) {
            Some(Some(Object::Enum { payload, .. })) => *payload,
            _ => None,
        }
    }
}

mod aggregate;
mod native_state;
mod seam;

pub use aggregate::AggregateMismatch;

#[cfg(test)]
#[path = "value_tests.rs"]
mod tests;
