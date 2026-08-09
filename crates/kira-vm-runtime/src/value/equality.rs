//! What it means for two heap values to be equal.
//!
//! Split from `value.rs` because it is a different question from allocation:
//! everything there hands out or reclaims storage, and this only *reads* it.
//! Equality follows handles rather than comparing them — two arrays with the
//! same elements are equal though they are different objects — so it walks the
//! same nesting the copy and drop paths do, and for the same reason it is
//! bounded: a payload is a value analysis resolved against types that already
//! resolve, so a cycle is unrepresentable.

use super::{EnumId, Heap, NativeStateValue, Object, Value};

impl Heap {
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
        // A deferred read compares as what it was read as. It cannot reach here
        // through `EqAny` — erasure rebuilds one first ([`Heap::own`]) — but
        // answering by identity would be a wrong answer rather than a refused
        // one, and this comparison is meant to follow handles.
        match (left, right) {
            (Value::NativeSnapshot(a), Value::NativeSnapshot(b)) => {
                match (self.snapshot_node(a), self.snapshot_node(b)) {
                    (Some(one), Some(other)) => one == other,
                    _ => false,
                }
            }
            (Value::NativeSnapshot(a), other) => match self.snapshot_node(a) {
                Some(node) => self.value_equals_node(other, node),
                None => false,
            },
            (other, Value::NativeSnapshot(b)) => match self.snapshot_node(b) {
                Some(node) => self.value_equals_node(other, node),
                None => false,
            },
            _ => self.objects_equal(left, right),
        }
    }

    /// Whether a heap value equals a stored callback-state node.
    fn value_equals_node(&self, value: Value, node: &NativeStateValue) -> bool {
        match (value, node) {
            (Value::Int(a), NativeStateValue::Int(b)) => a == *b,
            (Value::Float(a), NativeStateValue::Float(b)) => a == *b,
            (Value::Bool(a), NativeStateValue::Bool(b)) => a == *b,
            (Value::RawPtr(a), NativeStateValue::RawPtr(b)) => a == *b,
            (Value::Str(a), NativeStateValue::String(b)) => self.get(a) == b,
            (Value::Struct(a), NativeStateValue::Struct(b)) => {
                let fields = self.fields(a);
                fields.len() == b.len()
                    && fields
                        .iter()
                        .zip(b.iter())
                        .all(|(&field, node)| self.value_equals_node(field, node))
            }
            (Value::Array(a), NativeStateValue::Array(b)) => {
                let elements = self.elements(a);
                elements.len() == b.len()
                    && elements
                        .iter()
                        .zip(b.iter())
                        .all(|(&element, node)| self.value_equals_node(element, node))
            }
            (Value::Enum(a), NativeStateValue::Enum { tag, payload }) => {
                self.enum_tag(a) == Some(*tag)
                    && match (self.enum_payload_ref(a), payload.as_deref()) {
                        (Some(one), Some(other)) => self.value_equals_node(one, other),
                        (None, None) => true,
                        _ => false,
                    }
            }
            _ => false,
        }
    }

    /// [`Heap::values_equal`] for two values that are both real objects.
    fn objects_equal(&self, left: Value, right: Value) -> bool {
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
    pub(crate) fn enum_payload_ref(&self, id: EnumId) -> Option<Value> {
        match self.slots.get(id.0 as usize) {
            Some(Some(Object::Enum { payload, .. })) => *payload,
            _ => None,
        }
    }
}
