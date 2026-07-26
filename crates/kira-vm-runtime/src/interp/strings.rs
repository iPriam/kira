//! The interpreter's string primitives.
//!
//! `charAt`, `substring`, and `indexOf` all index bytes — the same units
//! `StringLen` counts — and each drops the strings it read on every path out,
//! including the failing ones: a popped value is this VM's to own, so a trap
//! must not strand storage in a heap that outlives the call.

use crate::error::VmError;
use crate::interp::Vm;
use crate::value::Value;

impl Vm<'_> {
    // ----- string primitives -------------------------------------------

    /// `s.charAt(i)`: the byte at `index`, dropping the string on every path.
    pub(super) fn read_char_at(&mut self, base: Value, index: Value) -> Result<i64, VmError> {
        let read = self.byte_at(base, index);
        self.heap.drop_value(base);
        read
    }

    /// The byte lookup itself, leaving ownership to the caller.
    fn byte_at(&self, base: Value, index: Value) -> Result<i64, VmError> {
        let Value::Str(id) = base else {
            return Err(VmError::NotAString);
        };
        let Value::Int(at) = index else {
            return Err(VmError::TypeMismatch { expected: "Int" });
        };
        // A negative index is out of bounds like any other: a string has one
        // dimension and one way to be indexed off the end of, so one trap says
        // it — and saying it the same way on every engine is what makes the
        // failure comparable.
        if at < 0 {
            return Err(VmError::StringIndexOutOfBounds);
        }
        let bytes = self.heap.get(id).as_bytes();
        let at = usize::try_from(at).unwrap_or(usize::MAX);
        bytes
            .get(at)
            .map(|byte| i64::from(*byte))
            .ok_or(VmError::StringIndexOutOfBounds)
    }

    /// `s.substring(start, end)`: the half-open byte slice, dropping the
    /// original on every path.
    pub(super) fn carve_substring(
        &mut self,
        base: Value,
        start: Value,
        end: Value,
    ) -> Result<String, VmError> {
        let carved = self.slice_of(base, start, end);
        self.heap.drop_value(base);
        carved
    }

    /// The slice itself, leaving ownership to the caller.
    fn slice_of(&self, base: Value, start: Value, end: Value) -> Result<String, VmError> {
        let Value::Str(id) = base else {
            return Err(VmError::NotAString);
        };
        let (Value::Int(from), Value::Int(to)) = (start, end) else {
            return Err(VmError::TypeMismatch { expected: "Int" });
        };
        if from < 0 || to < 0 {
            return Err(VmError::StringIndexOutOfBounds);
        }
        if from > to {
            return Err(VmError::InvertedSubstring);
        }
        let text = self.heap.get(id);
        let from = usize::try_from(from).unwrap_or(usize::MAX);
        let to = usize::try_from(to).unwrap_or(usize::MAX);
        // A slice that would split a multi-byte character is out of bounds
        // rather than a panic: `get` is `None` for a boundary that is not a
        // character boundary, which is exactly the range no `String` can hold.
        text.get(from..to)
            .map(str::to_owned)
            .ok_or(VmError::StringIndexOutOfBounds)
    }

    /// `s.indexOf(needle)`: the first byte index, or `-1`, dropping both.
    pub(super) fn find_index_of(&mut self, base: Value, needle: Value) -> Result<i64, VmError> {
        let found = match (base, needle) {
            (Value::Str(haystack), Value::Str(pattern)) => {
                let position = self
                    .heap
                    .get(haystack)
                    .find(self.heap.get(pattern))
                    .and_then(|at| i64::try_from(at).ok())
                    .unwrap_or(-1);
                Ok(position)
            }
            _ => Err(VmError::NotAString),
        };
        self.heap.drop_value(base);
        self.heap.drop_value(needle);
        found
    }
}
