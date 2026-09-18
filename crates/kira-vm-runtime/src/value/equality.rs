//! What it means for two heap values to be equal.
//!
//! Split from `value.rs` because it is a different question from allocation:
//! everything there hands out or reclaims storage, and this only *reads* it.
//! Equality follows handles rather than comparing them — two arrays with the
//! same elements are equal though they are different objects — so it walks the
//! same nesting the copy and drop paths do, and for the same reason it is
//! bounded: a payload is a value analysis resolved against types that already
//! resolve, so a cycle is unrepresentable.

use std::cmp::Ordering;

use super::{EnumId, Heap, NativeStateValue, Object, Value};

/// The 64-bit FNV-1a offset basis, the seed a structural hash folds from.
const FNV_SEED: u64 = 0xcbf2_9ce4_8422_2325;
/// The 64-bit FNV-1a prime, multiplied in after each byte.
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

impl Heap {
    /// The structural hash of a value, what `HashValue` answers.
    ///
    /// The fold twin of [`Heap::values_equal`], over the same nesting: a struct
    /// folds its fields in order, an array its length then its elements, an enum
    /// its tag then its payload, each leaf an integer's bytes, a boolean's bit, or
    /// a string's bytes. One FNV-1a accumulator runs through all of them, so two
    /// values that are structurally equal fold the identical byte stream and hash
    /// the same — the contract `Hashable` keeps with `==`.
    ///
    /// Only a type the checker proved `Hashable` reaches here, so every leaf is
    /// one this can fold; a float or a decimal was refused at the call. The walk
    /// is defined here in Rust and the native backend calls the same bytes, so the
    /// two engines agree on the number.
    pub fn hash_value(&self, value: Value) -> i64 {
        let mut acc = FNV_SEED;
        self.hash_into(&mut acc, value);
        acc as i64
    }

    /// Folds one value's canonical bytes into the running FNV-1a accumulator.
    fn hash_into(&self, acc: &mut u64, value: Value) {
        fn feed(acc: &mut u64, bytes: &[u8]) {
            for &byte in bytes {
                *acc ^= u64::from(byte);
                *acc = acc.wrapping_mul(FNV_PRIME);
            }
        }
        match value {
            Value::Int(i) => feed(acc, &i.to_le_bytes()),
            Value::Bool(b) => feed(acc, &[u8::from(b)]),
            Value::Str(id) => feed(acc, self.get(id).as_bytes()),
            Value::Struct(id) => {
                for &field in self.fields(id) {
                    self.hash_into(acc, field);
                }
            }
            Value::Array(id) => {
                let elements = self.elements(id);
                feed(acc, &(elements.len() as u64).to_le_bytes());
                for &element in elements {
                    self.hash_into(acc, element);
                }
            }
            Value::Enum(id) => {
                let tag = self.enum_tag(id).unwrap_or(0);
                feed(acc, &tag.to_le_bytes());
                if let Some(payload) = self.enum_payload_ref(id) {
                    self.hash_into(acc, payload);
                }
            }
            // Unreachable: `Hashable` admits only the leaves above. Folding
            // nothing for an unexpected kind is the one total, side-effect-free
            // choice if one ever arrived.
            _ => {}
        }
    }

    /// The structural three-way order of two values, what `CmpValue` answers.
    ///
    /// The ordering twin of [`Heap::values_equal`], walking the same nesting:
    /// two structs compare field-by-field in declaration order, two arrays
    /// element-by-element then by length, two enums by tag then payload, each
    /// leaf bottoming out at a scalar, a string's bytes, or a decimal's value.
    /// The first pair that is not `Equal` decides; equal pairs walk on. So a
    /// value and an independent copy of it compare `Equal`, exactly as they
    /// compare equal.
    ///
    /// Only the type checker's `Ordered` conformance reaches here, so every leaf
    /// is one this can total-order: a leaf with no order — a pointer word, a
    /// handle, bare native state — was refused at the comparison site and never
    /// arrives. A `Float` orders by the ordered comparisons `<` and `>`, so a
    /// `NaN` (unordered against everything) falls into the `Equal` bucket rather
    /// than trapping, matching what the native backend's `fcmp` emits.
    ///
    /// Bounded by the value's nesting depth for the same reason
    /// [`Heap::values_equal`] is: a cycle is unrepresentable.
    pub fn compare_values(&self, left: Value, right: Value) -> Ordering {
        match (left, right) {
            (Value::Int(a), Value::Int(b)) => a.cmp(&b),
            (Value::Float(a), Value::Float(b)) => {
                if a < b {
                    Ordering::Less
                } else if a > b {
                    Ordering::Greater
                } else {
                    Ordering::Equal
                }
            }
            (Value::Bool(a), Value::Bool(b)) => a.cmp(&b),
            (Value::Number(a), Value::Number(b)) => a.compare(b).unwrap_or(Ordering::Equal),
            (Value::Str(a), Value::Str(b)) => self.get(a).cmp(self.get(b)),
            (Value::Struct(a), Value::Struct(b)) => {
                let (left, right) = (self.fields(a), self.fields(b));
                for (&one, &other) in left.iter().zip(right.iter()) {
                    let order = self.compare_values(one, other);
                    if order != Ordering::Equal {
                        return order;
                    }
                }
                Ordering::Equal
            }
            (Value::Array(a), Value::Array(b)) => {
                let (left, right) = (self.elements(a), self.elements(b));
                for (&one, &other) in left.iter().zip(right.iter()) {
                    let order = self.compare_values(one, other);
                    if order != Ordering::Equal {
                        return order;
                    }
                }
                left.len().cmp(&right.len())
            }
            (Value::Enum(a), Value::Enum(b)) => match self.enum_tag(a).cmp(&self.enum_tag(b)) {
                Ordering::Equal => match (self.enum_payload_ref(a), self.enum_payload_ref(b)) {
                    (Some(one), Some(other)) => self.compare_values(one, other),
                    _ => Ordering::Equal,
                },
                order => order,
            },
            // Unreachable: `Ordered` conformance admits only the leaves above, so
            // a mismatched or unordered kind never arrives. Answering `Equal` is
            // the one total, side-effect-free choice if one ever did.
            _ => Ordering::Equal,
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
                self.enum_tag(a) == Some(u64::from(*tag))
                    && match (self.enum_payload_ref(a), payload.as_deref()) {
                        (Some(one), Some(other)) => self.value_equals_node(one, other),
                        (None, None) => true,
                        _ => false,
                    }
            }
            (Value::Erased(a), NativeStateValue::Any { type_id, payload }) => {
                self.erased_type_id(a) == Some(*type_id)
                    && self
                        .erased_payload_ref(a)
                        .is_some_and(|value| self.value_equals_node(value, payload))
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
            // A C block compares as the pointer word C would read — its
            // payload address — against another block or a bare word. Two
            // blocks are two allocations, so equality here is identity, which
            // is exactly what comparing the same members on native computes.
            (Value::CBlock(_), _) | (_, Value::CBlock(_)) => {
                match (self.seam_word(left), self.seam_word(right)) {
                    (Value::RawPtr(a), Value::RawPtr(b)) => a == b,
                    _ => false,
                }
            }
            (Value::Void, Value::Void) => true,
            // A decimal compares by its exact value, the same equality
            // `NumberOp::Equal` gives at the top level.
            (Value::Number(a), Value::Number(b)) => a.equals(b).unwrap_or(false),
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
            (Value::MainThreadTask(a), Value::MainThreadTask(b)) => a == b,
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
