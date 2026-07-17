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

use kira_runtime_abi::{NativeArg, NativeResult};

/// A handle to a heap-allocated string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StrId(u32);

/// A handle to a heap-allocated struct value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StructId(u32);

/// A handle to a heap-allocated array value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArrayId(u32);

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
            scalar => scalar,
        }
    }

    /// Renders a value as the text `print` emits, consuming what it owns, or
    /// `None` when the value has no pinned rendering.
    ///
    /// Float formatting matches the reference: whole floats print without a
    /// decimal point (`2.0` -> `2`), matching Rust's default `f64` display.
    ///
    /// A struct is the `None` case, and deliberately so: what `print` renders
    /// for a struct is not pinned anywhere in the language corpus, so any text
    /// invented here would be inventing language surface. Analysis rejects
    /// `print(someStruct)` before a program runs; this is the runtime saying
    /// the same thing rather than printing something made up.
    pub fn format_and_consume(&mut self, value: Value) -> Option<String> {
        let rendered = match value {
            Value::Int(n) => n.to_string(),
            Value::Float(f) => f.to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Str(id) => self.get(id).to_owned(),
            Value::Void => String::new(),
            // A struct and an array are both the `None` case, and for the same
            // reason: neither has a rendering the language corpus pins, so any
            // text invented here would be inventing language surface. Analysis
            // rejects both before a program runs; this is the runtime saying
            // the same thing rather than printing something made up.
            Value::Struct(_) | Value::Array(_) => {
                self.drop_value(value);
                return None;
            }
        };
        self.drop_value(value);
        Some(rendered)
    }

    /// Brings a seam argument into this heap as a runtime value.
    ///
    /// The seam's rule is that arguments borrow, so a string is copied in here
    /// rather than aliased: the caller's storage stays the caller's, and the
    /// value this returns is this heap's to drop like any other.
    pub fn lower(&mut self, argument: NativeArg<'_>) -> Value {
        match argument {
            NativeArg::Void => Value::Void,
            NativeArg::Int(value) => Value::Int(value),
            NativeArg::Float(value) => Value::Float(value),
            NativeArg::Bool(value) => Value::Bool(value),
            NativeArg::Str(text) => Value::Str(self.alloc(text.to_owned())),
        }
    }

    /// Takes an owned seam result into this heap as a runtime value.
    ///
    /// The seam's rule is that results own, so a returned string is *moved* in
    /// rather than copied: nothing else holds it.
    pub fn absorb(&mut self, result: NativeResult) -> Value {
        match result {
            NativeResult::Void => Value::Void,
            NativeResult::Int(value) => Value::Int(value),
            NativeResult::Float(value) => Value::Float(value),
            NativeResult::Bool(value) => Value::Bool(value),
            NativeResult::Str(text) => Value::Str(self.alloc(text)),
        }
    }

    /// Renders a runtime value as a seam result, leaving `value` untouched, or
    /// `None` when the value has no representation at the seam.
    ///
    /// The seam's rule is that results own, so a string is copied out: the
    /// result outlives this heap, and the caller drops `value` itself.
    ///
    /// A struct is the `None` case: [`NativeResult`] has no struct shape, and
    /// the hybrid ABI has no layout for one yet. This says so rather than
    /// substituting some other value — a wrong answer here is a wrong answer
    /// about *ownership*, which is a double free or a leak at the boundary, not
    /// a bad print. The signature split is checked before a hybrid program is
    /// ever built, so a rejected value should never reach here.
    pub fn lift(&self, value: Value) -> Option<NativeResult> {
        Some(match value {
            Value::Void => NativeResult::Void,
            Value::Int(value) => NativeResult::Int(value),
            Value::Float(value) => NativeResult::Float(value),
            Value::Bool(value) => NativeResult::Bool(value),
            Value::Str(id) => NativeResult::Str(self.get(id).to_owned()),
            Value::Struct(_) | Value::Array(_) => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alloc_free_balances_and_reuses_slots() {
        let mut heap = Heap::new();
        let a = heap.alloc("one".to_owned());
        let b = heap.alloc("two".to_owned());
        assert_eq!(heap.stats().current, 2);
        heap.free(a);
        assert_eq!(heap.stats().current, 1);
        // Freed slot is reused, so the id index recycles.
        let c = heap.alloc("three".to_owned());
        assert_eq!(heap.get(c), "three");
        assert_eq!(heap.get(b), "two");
        assert_eq!(heap.stats().allocated, 3);
        assert_eq!(heap.stats().freed, 1);
    }

    #[test]
    fn copy_of_a_string_is_independent() {
        let mut heap = Heap::new();
        let a = heap.alloc("x".to_owned());
        let copy = heap.copy_value(Value::Str(a));
        assert_eq!(heap.stats().current, 2);
        heap.drop_value(Value::Str(a));
        // The copy survives the original's drop.
        assert_eq!(heap.stats().current, 1);
        assert!(matches!(copy, Value::Str(_)));
    }

    #[test]
    fn float_formatting_drops_trailing_zero() {
        let mut heap = Heap::new();
        assert_eq!(
            heap.format_and_consume(Value::Float(2.0)).as_deref(),
            Some("2")
        );
        assert_eq!(
            heap.format_and_consume(Value::Float(3.5)).as_deref(),
            Some("3.5")
        );
        assert_eq!(
            heap.format_and_consume(Value::Int(-7)).as_deref(),
            Some("-7")
        );
        assert_eq!(
            heap.format_and_consume(Value::Bool(true)).as_deref(),
            Some("true")
        );
    }

    #[test]
    fn a_struct_has_no_invented_rendering_or_seam_shape() {
        let mut heap = Heap::new();
        let value = Value::Struct(heap.alloc_struct(vec![Value::Int(1)]));
        assert_eq!(heap.lift(value), None);
        // Formatting still consumes what it was handed, so refusing to render
        // a struct does not leak it.
        assert_eq!(heap.format_and_consume(value), None);
        assert_eq!(heap.stats().current, 0);
    }

    #[test]
    fn freeing_a_struct_frees_its_fields() {
        let mut heap = Heap::new();
        let text = heap.alloc("label".to_owned());
        let inner = heap.alloc_struct(vec![Value::Str(text)]);
        let outer = heap.alloc_struct(vec![Value::Struct(inner), Value::Int(7)]);
        assert_eq!(heap.stats().current, 3);
        heap.drop_value(Value::Struct(outer));
        // The string, the inner struct, and the outer struct all go.
        assert_eq!(heap.stats().current, 0);
    }

    #[test]
    fn copying_a_struct_is_deep_so_writes_do_not_alias() {
        let mut heap = Heap::new();
        let text = heap.alloc("a".to_owned());
        let original = heap.alloc_struct(vec![Value::Str(text)]);
        let Value::Struct(copy) = heap.copy_value(Value::Struct(original)) else {
            panic!("a struct copies to a struct");
        };
        assert_ne!(original, copy, "a copy is its own object");

        // Overwrite the copy's string; the original must not see it.
        let replacement = heap.alloc("b".to_owned());
        assert!(heap.set_field(copy, 0, Value::Str(replacement)));
        let Some(Value::Str(original_text)) = heap.field(original, 0) else {
            panic!("the original still holds its string");
        };
        assert_eq!(heap.get(original_text), "a");

        heap.drop_value(Value::Struct(original));
        heap.drop_value(Value::Struct(copy));
        assert_eq!(heap.stats().current, 0, "no field is freed twice or leaked");
    }

    #[test]
    fn overwriting_a_field_drops_what_was_there() {
        let mut heap = Heap::new();
        let text = heap.alloc("gone".to_owned());
        let id = heap.alloc_struct(vec![Value::Str(text)]);
        assert_eq!(heap.stats().current, 2);
        assert!(heap.set_field(id, 0, Value::Int(1)));
        // The replaced string is freed, not leaked: only the struct is live.
        assert_eq!(heap.stats().current, 1);
        heap.drop_value(Value::Struct(id));
        assert_eq!(heap.stats().current, 0);
    }

    #[test]
    fn freeing_an_array_frees_its_elements() {
        let mut heap = Heap::new();
        let text = heap.alloc("label".to_owned());
        let inner = heap.alloc_array(vec![Value::Str(text)]);
        let outer = heap.alloc_array(vec![Value::Array(inner), Value::Int(7)]);
        assert_eq!(heap.stats().current, 3);
        heap.drop_value(Value::Array(outer));
        assert_eq!(heap.stats().current, 0);
    }

    #[test]
    fn copying_an_array_is_deep_so_writes_do_not_alias() {
        let mut heap = Heap::new();
        let text = heap.alloc("a".to_owned());
        let original = heap.alloc_array(vec![Value::Str(text)]);
        let Value::Array(copy) = heap.copy_value(Value::Array(original)) else {
            panic!("an array copies to an array");
        };
        assert_ne!(original, copy, "a copy is its own object");

        let replacement = heap.alloc("b".to_owned());
        assert!(heap.set_element(copy, 0, Value::Str(replacement)));
        let Some(Value::Str(original_text)) = heap.element(original, 0) else {
            panic!("the original still holds its string");
        };
        assert_eq!(heap.get(original_text), "a");

        heap.drop_value(Value::Array(original));
        heap.drop_value(Value::Array(copy));
        assert_eq!(
            heap.stats().current,
            0,
            "no element is freed twice or leaked"
        );
    }

    /// The question the array design turned on: copying a struct that holds an
    /// array must copy the array, not share the handle. It falls out of the
    /// recursion — the struct arm copies each field — but it is the behaviour
    /// the whole ownership story rests on, so it is pinned directly rather than
    /// inferred from the code.
    #[test]
    fn copying_a_struct_deep_copies_an_array_field() {
        let mut heap = Heap::new();
        let values = heap.alloc_array(vec![Value::Int(1), Value::Int(2)]);
        let original = heap.alloc_struct(vec![Value::Array(values)]);

        let Value::Struct(copy) = heap.copy_value(Value::Struct(original)) else {
            panic!("a struct copies to a struct");
        };
        let Some(Value::Array(copied_values)) = heap.field(copy, 0) else {
            panic!("the copy holds an array");
        };
        assert_ne!(
            values, copied_values,
            "the copy's array is its own object, not a shared handle"
        );

        // Mutating the copy's array must leave the original's alone.
        assert!(heap.set_element(copied_values, 0, Value::Int(99)));
        assert_eq!(heap.element(values, 0), Some(Value::Int(1)));
        assert_eq!(heap.element(copied_values, 0), Some(Value::Int(99)));

        // …and growing it must not grow the original's either.
        assert!(heap.push_element(copied_values, Value::Int(3)));
        assert_eq!(heap.array_len(values), Some(2));
        assert_eq!(heap.array_len(copied_values), Some(3));

        heap.drop_value(Value::Struct(original));
        heap.drop_value(Value::Struct(copy));
        assert_eq!(heap.stats().current, 0);
    }

    #[test]
    fn overwriting_an_element_drops_what_was_there() {
        let mut heap = Heap::new();
        let text = heap.alloc("gone".to_owned());
        let id = heap.alloc_array(vec![Value::Str(text)]);
        assert_eq!(heap.stats().current, 2);
        assert!(heap.set_element(id, 0, Value::Int(1)));
        // The replaced string is freed, not leaked: only the array is live.
        assert_eq!(heap.stats().current, 1);
        heap.drop_value(Value::Array(id));
        assert_eq!(heap.stats().current, 0);
    }

    #[test]
    fn appending_grows_the_array_in_place() {
        let mut heap = Heap::new();
        let id = heap.alloc_array(Vec::new());
        assert_eq!(heap.array_len(id), Some(0));
        assert!(heap.push_element(id, Value::Int(1)));
        assert!(heap.push_element(id, Value::Int(2)));
        assert_eq!(heap.array_len(id), Some(2));
        assert_eq!(heap.element(id, 1), Some(Value::Int(2)));
        heap.drop_value(Value::Array(id));
        assert_eq!(heap.stats().current, 0);
    }

    #[test]
    fn an_array_has_no_invented_rendering_or_seam_shape() {
        let mut heap = Heap::new();
        let value = Value::Array(heap.alloc_array(vec![Value::Int(1)]));
        assert_eq!(heap.lift(value), None);
        // Refusing to render one still consumes it, so it does not leak.
        assert_eq!(heap.format_and_consume(value), None);
        assert_eq!(heap.stats().current, 0);
    }

    #[test]
    fn a_handle_of_the_wrong_kind_reads_empty_rather_than_panicking() {
        let mut heap = Heap::new();
        let text = heap.alloc("x".to_owned());
        // A struct handle over a string slot: the VM must not panic on it.
        assert_eq!(heap.fields(StructId(text.0)), &[]);
        assert_eq!(heap.field(StructId(text.0), 0), None);
        assert!(!heap.set_field(StructId(text.0), 0, Value::Int(1)));
        // …and the string is untouched by any of it.
        assert_eq!(heap.get(text), "x");
    }
}
