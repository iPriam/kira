//! The interpreter's string primitives.
//!
//! `charAt`, `substring`, and `indexOf` all index bytes — the same units
//! `StringLen` counts — and each drops every operand it read on every path
//! out, including the failing ones: a popped value is this VM's to own, so a
//! trap must not strand storage in a heap that outlives the call.

use kira_runtime_abi::StringOp;

use crate::error::VmError;
use crate::interp::Vm;
use crate::value::Value;

impl Vm<'_> {
    // ----- string primitives -------------------------------------------

    /// `s.charAt(i)`: the byte at `index`, dropping both operands on every
    /// path.
    pub(super) fn read_char_at(&mut self, base: Value, index: Value) -> Result<i64, VmError> {
        let read = self.byte_at(base, index);
        self.heap.drop_value(base);
        self.heap.drop_value(index);
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

    /// `s.substring(start, end)`: the half-open byte slice, dropping all
    /// three operands on every path.
    pub(super) fn carve_substring(
        &mut self,
        base: Value,
        start: Value,
        end: Value,
    ) -> Result<String, VmError> {
        let carved = self.slice_of(base, start, end);
        self.heap.drop_value(base);
        self.heap.drop_value(start);
        self.heap.drop_value(end);
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

    /// One of the shared-opcode string operations.
    ///
    /// `arguments` are in source order and `base` is the receiver; this owns
    /// all of them and drops every one on every path out, failing ones
    /// included, exactly as the primitives above do.
    ///
    /// The searching operations lean on `str::find`, which is a two-way search
    /// with a memchr-accelerated scan underneath — so the substring hunt is
    /// already vectorized where the platform allows, without this file knowing
    /// anything about vectors.
    pub(super) fn perform_string_op(
        &mut self,
        op: StringOp,
        base: Value,
        arguments: &[Value],
    ) -> Result<Value, VmError> {
        let performed = self.string_op_result(op, base, arguments);
        self.heap.drop_value(base);
        for &argument in arguments {
            self.heap.drop_value(argument);
        }
        performed
    }

    /// The operation itself, leaving ownership to the caller.
    fn string_op_result(
        &mut self,
        op: StringOp,
        base: Value,
        arguments: &[Value],
    ) -> Result<Value, VmError> {
        let Value::Str(id) = base else {
            return Err(VmError::NotAString);
        };
        let text = self.heap.get(id);
        // Every argument is a `String`; validate that without building a
        // temporary `Vec<&str>`. The operation/arity match below can then borrow
        // each string directly from the heap.
        if arguments
            .iter()
            .any(|argument| !matches!(argument, Value::Str(_)))
        {
            return Err(VmError::NotAString);
        }
        match (op, arguments) {
            (StringOp::Contains, [Value::Str(needle)]) => {
                Ok(Value::Bool(text.contains(self.heap.get(*needle))))
            }
            (StringOp::StartsWith, [Value::Str(prefix)]) => {
                Ok(Value::Bool(text.starts_with(self.heap.get(*prefix))))
            }
            (StringOp::EndsWith, [Value::Str(suffix)]) => {
                Ok(Value::Bool(text.ends_with(self.heap.get(*suffix))))
            }
            (StringOp::Replace, [Value::Str(from), Value::Str(to)]) => {
                let replaced = text.replace(self.heap.get(*from), self.heap.get(*to));
                Ok(Value::Str(self.heap.alloc(replaced)))
            }
            (StringOp::IsInt, []) => Ok(Value::Bool(text.trim().parse::<i64>().is_ok())),
            (StringOp::ToInt, []) => match text.trim().parse::<i64>() {
                Ok(value) => Ok(Value::Int(value)),
                Err(_) => Err(VmError::NotAWholeNumber),
            },
            (StringOp::DropLastScalar, []) => {
                let mut dropped = text.to_owned();
                dropped.pop();
                Ok(Value::Str(self.heap.alloc(dropped)))
            }
            (StringOp::Trim, []) => {
                let trimmed = text.trim().to_owned();
                Ok(Value::Str(self.heap.alloc(trimmed)))
            }
            (StringOp::Lowercase, []) => {
                let lowered = text.to_lowercase();
                Ok(Value::Str(self.heap.alloc(lowered)))
            }
            (StringOp::Uppercase, []) => {
                let raised = text.to_uppercase();
                Ok(Value::Str(self.heap.alloc(raised)))
            }
            (StringOp::Split, [Value::Str(separator)]) => {
                // An empty separator would make `split` yield one empty piece
                // per character boundary plus two ends, which is not a split of
                // anything. The whole text as one piece is the honest answer to
                // "split this on nothing".
                let separator = self.heap.get(*separator);
                let pieces: Vec<String> = if separator.is_empty() {
                    vec![text.to_owned()]
                } else {
                    text.split(separator).map(str::to_owned).collect()
                };
                let elements: Vec<Value> = pieces
                    .into_iter()
                    .map(|piece| Value::Str(self.heap.alloc(piece)))
                    .collect();
                Ok(Value::Array(self.heap.alloc_array(elements)))
            }
            // The arity was fixed by analysis and by the operand byte, so a
            // mismatch here is a malformed module rather than a program error.
            _ => Err(VmError::TypeMismatch { expected: "String" }),
        }
    }
}
