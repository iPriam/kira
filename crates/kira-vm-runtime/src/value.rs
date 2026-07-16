//! Runtime values and the string heap with affine drop accounting.
//!
//! Scalars (`Int`, `Float`, `Bool`, `Void`) are `Copy` and live inline in a
//! [`Value`]. Strings live on a [`Heap`]; a [`Value::Str`] is a handle into it.
//! Strings follow value semantics with affine drops: reading a local *copies*
//! its string (a fresh allocation) and every instruction that consumes a string
//! *frees* it, so a well-formed run ends with [`HeapStats::current`] at zero.

use kira_runtime_abi::{NativeArg, NativeResult};

/// A handle to a heap-allocated string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StrId(u32);

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
    /// The unit value.
    Void,
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

/// The string heap: owns every live string and counts allocations and frees.
#[derive(Debug, Default)]
pub struct Heap {
    slots: Vec<Option<String>>,
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
        self.allocated += 1;
        if let Some(index) = self.free_list.pop() {
            self.slots[index as usize] = Some(value);
            StrId(index)
        } else {
            let index = self.slots.len() as u32;
            self.slots.push(Some(value));
            StrId(index)
        }
    }

    /// Borrows the string behind a handle.
    ///
    /// Returns `""` for a handle whose slot was already freed, so a
    /// misbehaving caller cannot panic the VM.
    pub fn get(&self, id: StrId) -> &str {
        self.slots
            .get(id.0 as usize)
            .and_then(|slot| slot.as_deref())
            .unwrap_or("")
    }

    /// Frees the string behind a handle and records the free.
    pub fn free(&mut self, id: StrId) {
        if let Some(slot) = self.slots.get_mut(id.0 as usize)
            && slot.take().is_some()
        {
            self.freed += 1;
            self.free_list.push(id.0);
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

    /// Drops a value, freeing its string when it owns one.
    pub fn drop_value(&mut self, value: Value) {
        if let Value::Str(id) = value {
            self.free(id);
        }
    }

    /// Produces an independent copy of a value (clone-on-read for strings).
    pub fn copy_value(&mut self, value: Value) -> Value {
        match value {
            Value::Str(id) => {
                let cloned = self.get(id).to_owned();
                Value::Str(self.alloc(cloned))
            }
            scalar => scalar,
        }
    }

    /// Renders a value as the text `print` emits, consuming any string it owns.
    ///
    /// Float formatting matches the reference: whole floats print without a
    /// decimal point (`2.0` -> `2`), matching Rust's default `f64` display.
    pub fn format_and_consume(&mut self, value: Value) -> String {
        let rendered = match value {
            Value::Int(n) => n.to_string(),
            Value::Float(f) => f.to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Str(id) => self.get(id).to_owned(),
            Value::Void => String::new(),
        };
        self.drop_value(value);
        rendered
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

    /// Renders a runtime value as a seam result, leaving `value` untouched.
    ///
    /// The seam's rule is that results own, so a string is copied out: the
    /// result outlives this heap, and the caller drops `value` itself.
    pub fn lift(&self, value: Value) -> NativeResult {
        match value {
            Value::Void => NativeResult::Void,
            Value::Int(value) => NativeResult::Int(value),
            Value::Float(value) => NativeResult::Float(value),
            Value::Bool(value) => NativeResult::Bool(value),
            Value::Str(id) => NativeResult::Str(self.get(id).to_owned()),
        }
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
        assert_eq!(heap.format_and_consume(Value::Float(2.0)), "2");
        assert_eq!(heap.format_and_consume(Value::Float(3.5)), "3.5");
        assert_eq!(heap.format_and_consume(Value::Int(-7)), "-7");
        assert_eq!(heap.format_and_consume(Value::Bool(true)), "true");
    }
}
