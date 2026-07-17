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
            _ => {}
        }
    }

    /// Produces an independent copy of a value.
    ///
    /// Deep by construction: a struct's copy owns fresh copies of its fields,
    /// so no two live values ever share heap storage. That is what makes
    /// `var b = a; b.x = 1` leave `a` alone.
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
            Value::Struct(_) => {
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
            Value::Struct(_) => return None,
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
