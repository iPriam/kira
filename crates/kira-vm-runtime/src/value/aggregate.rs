//! Marshalling a Kira struct into C-layout bytes and back at the foreign seam.
//!
//! A `@FFI.Struct { layout: c }` crossing by value is still an ordinary Kira
//! struct on this side — a heap object with a field list. What crosses is a
//! block of bytes laid out the way the C compiler lays out the struct the
//! generated shim declares.
//!
//! # Why the tree, walked twice
//!
//! Both directions walk the aggregate's member tree and the struct's fields in
//! lockstep: member `n` of the aggregate pairs with field `n` of the struct, and
//! a nested aggregate member recurses into a nested `Value::Struct`. That is why
//! the table stores a tree rather than a flat leaf list — a flat list can say
//! where each scalar goes, but not how to rebuild the nesting a Kira value has.
//!
//! A field that does not match its member's shape is refused rather than
//! guessed at. The frontend built the table from this very struct, so a shape
//! mismatch is a compiler bug; an array longer than the C extent it fills is
//! not, and the two are reported apart so only the second is explained to the
//! author. Both become a typed trap in the interpreter.

use kira_runtime_abi::c_storage::{bool_from_c_byte, c_bool_byte};
use kira_runtime_abi::{
    ForeignAggregateId, ForeignAggregates, ForeignArrayElement, ForeignLayout, ForeignMember,
    ForeignPointerWidth, ForeignType, scalar_layout,
};

use super::{ArrayId, CBlockId, Heap, Object, StructId, Value};

mod cblock;

/// Why a Kira value could not be written as C-layout bytes.
///
/// Two reasons, kept apart because they are different mistakes: a shape that
/// disagrees with the table is a compiler bug, while an array longer than the
/// C extent is a program the author wrote — and only the second can be
/// explained to them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggregateMismatch {
    /// The value is not the shape the aggregate describes.
    Shape,
    /// A Kira array held more elements than the inline C array reserves.
    ArrayTooLong {
        /// The C extent, in elements.
        count: u32,
        /// The Kira array's length.
        len: usize,
    },
}

/// Reads an absent step of the walk as a shape disagreement.
///
/// Every one of them is the table and the value describing different things,
/// which the frontend built together — so it is a compiler bug, and named as
/// one rather than explained to the author.
fn shape<T>(step: Option<T>) -> Result<T, AggregateMismatch> {
    step.ok_or(AggregateMismatch::Shape)
}

/// Rounds `value` up to the next multiple of `align`.
///
/// The same rule the layout pass uses; `align` is always a power of two from
/// [`scalar_layout`] or a computed aggregate layout, so this never divides by
/// zero.
fn round_up(value: u32, align: u32) -> Option<u32> {
    value
        .checked_add(align - 1)
        .map(|raised| raised - (raised % align))
}

/// The context one marshalling walk carries unchanged from top to bottom.
///
/// The table and the target width are the same at every level, and the layouts
/// are computed once for the whole table rather than per nested member — so
/// they travel together instead of as three parameters threaded through every
/// recursive call.
struct Walk<'a> {
    table: &'a ForeignAggregates,
    layouts: Vec<ForeignLayout>,
    width: ForeignPointerWidth,
}

impl<'a> Walk<'a> {
    /// Prepares a walk over `table` for `width`, laying every aggregate out once.
    fn new(table: &'a ForeignAggregates, width: ForeignPointerWidth) -> Option<Self> {
        Some(Self {
            layouts: table.layouts(width).ok()?,
            table,
            width,
        })
    }

    /// The layout of one aggregate in this walk.
    fn layout(&self, id: ForeignAggregateId) -> Option<ForeignLayout> {
        self.layouts.get(id.0 as usize).copied()
    }

    /// The stride and alignment of one inline-array element.
    ///
    /// The stride is the element's own size: C puts nothing between array
    /// elements, because a size is already a multiple of its alignment.
    fn element_layout(&self, element: ForeignArrayElement) -> Option<(u32, u32)> {
        let layout = match element {
            ForeignArrayElement::Scalar(ty) => scalar_layout(ty, self.width),
            ForeignArrayElement::Aggregate(id) => self.layout(id)?,
        };
        Some((layout.size, layout.align))
    }
}

impl Heap {
    /// Writes `value` into `sizeof(aggregate)` bytes in the target's C layout.
    ///
    /// [`AggregateMismatch::Shape`] when the value is not a struct of the shape
    /// the aggregate describes, or when the table cannot be laid out.
    pub fn aggregate_bytes(
        &self,
        table: &ForeignAggregates,
        id: ForeignAggregateId,
        value: Value,
        width: ForeignPointerWidth,
    ) -> Result<Vec<u8>, AggregateMismatch> {
        let walk = Walk::new(table, width).ok_or(AggregateMismatch::Shape)?;
        let layout = walk.layout(id).ok_or(AggregateMismatch::Shape)?;
        let mut bytes = vec![0u8; layout.size as usize];
        self.write_aggregate(&walk, id, value, 0, &mut bytes)?;
        Ok(bytes)
    }

    /// Writes one aggregate's members into `bytes` at `base`.
    fn write_aggregate(
        &self,
        walk: &Walk<'_>,
        id: ForeignAggregateId,
        value: Value,
        base: u32,
        bytes: &mut [u8],
    ) -> Result<(), AggregateMismatch> {
        let aggregate = walk.table.get(id).ok_or(AggregateMismatch::Shape)?;
        let Value::Struct(handle) = value else {
            return Err(AggregateMismatch::Shape);
        };
        let fields = self.fields(handle);
        if fields.len() != aggregate.members().len() {
            return Err(AggregateMismatch::Shape);
        }
        let mut offset = 0u32;
        for (member, field) in aggregate.members().iter().zip(fields.iter().copied()) {
            match member {
                ForeignMember::Scalar(ty) => {
                    let layout = scalar_layout(*ty, walk.width);
                    offset = shape(round_up(offset, layout.align))?;
                    let at = shape(base.checked_add(offset))? as usize;
                    let encoded = shape(encode_scalar(*ty, self.seam_word(field), walk.width))?;
                    shape(bytes.get_mut(at..at + encoded.len()))?.copy_from_slice(&encoded);
                    offset = shape(offset.checked_add(layout.size))?;
                }
                ForeignMember::Aggregate(nested) => {
                    let layout = shape(walk.layout(*nested))?;
                    offset = shape(round_up(offset, layout.align))?;
                    let at = shape(base.checked_add(offset))?;
                    self.write_aggregate(walk, *nested, field, at, bytes)?;
                    offset = shape(offset.checked_add(layout.size))?;
                }
                ForeignMember::Array { element, count } => {
                    let (stride, align) = shape(walk.element_layout(*element))?;
                    offset = shape(round_up(offset, align))?;
                    let at = shape(base.checked_add(offset))?;
                    self.write_array(walk, *element, *count, stride, field, at, bytes)?;
                    offset = shape(offset.checked_add(shape(stride.checked_mul(*count))?))?;
                }
            }
        }
        Ok(())
    }

    /// Writes a Kira array field into `count` inline elements at `base`.
    ///
    /// A shorter Kira array fills the elements it has and leaves the rest as the
    /// zeroes the buffer started with — which is what C sees for a partially
    /// filled fixed array, and what a zero-filled construction means. A longer
    /// one is [`AggregateMismatch::ArrayTooLong`]: silently dropping elements
    /// would hand C a different value than the program wrote.
    #[allow(clippy::too_many_arguments)]
    fn write_array(
        &self,
        walk: &Walk<'_>,
        element: ForeignArrayElement,
        count: u32,
        stride: u32,
        field: Value,
        base: u32,
        bytes: &mut [u8],
    ) -> Result<(), AggregateMismatch> {
        let Value::Array(handle) = field else {
            return Err(AggregateMismatch::Shape);
        };
        let elements = self.elements(handle).to_vec();
        if elements.len() > count as usize {
            return Err(AggregateMismatch::ArrayTooLong {
                count,
                len: elements.len(),
            });
        }
        for (index, value) in elements.into_iter().enumerate() {
            let at = shape(base.checked_add(shape(stride.checked_mul(index as u32))?))?;
            match element {
                ForeignArrayElement::Scalar(ty) => {
                    let encoded = shape(encode_scalar(ty, self.seam_word(value), walk.width))?;
                    let at = at as usize;
                    shape(bytes.get_mut(at..at + encoded.len()))?.copy_from_slice(&encoded);
                }
                ForeignArrayElement::Aggregate(nested) => {
                    self.write_aggregate(walk, nested, value, at, bytes)?;
                }
            }
        }
        Ok(())
    }

    /// Rebuilds a Kira struct from `sizeof(aggregate)` C-layout bytes.
    ///
    /// `None` when the bytes are the wrong length or the table cannot be laid
    /// out. Every value allocated on the way is reachable from the returned
    /// one, so a partial failure cannot strand a heap object: the recursion
    /// builds each nested struct completely before its parent takes it.
    pub fn absorb_aggregate(
        &mut self,
        table: &ForeignAggregates,
        id: ForeignAggregateId,
        bytes: &[u8],
        width: ForeignPointerWidth,
    ) -> Option<Value> {
        let walk = Walk::new(table, width)?;
        if bytes.len() != walk.layout(id)?.size as usize {
            return None;
        }
        self.read_aggregate(&walk, id, bytes, 0)
    }

    /// Reads one aggregate's members out of `bytes` at `base`.
    fn read_aggregate(
        &mut self,
        walk: &Walk<'_>,
        id: ForeignAggregateId,
        bytes: &[u8],
        base: u32,
    ) -> Option<Value> {
        let members = walk.table.get(id)?.members().to_vec();
        let mut fields = Vec::with_capacity(members.len());
        let mut offset = 0u32;
        for member in &members {
            match member {
                ForeignMember::Scalar(ty) => {
                    let layout = scalar_layout(*ty, walk.width);
                    offset = round_up(offset, layout.align)?;
                    let at = base.checked_add(offset)? as usize;
                    let slice = bytes.get(at..at + layout.size as usize)?;
                    fields.push(decode_scalar(*ty, slice)?);
                    offset = offset.checked_add(layout.size)?;
                }
                ForeignMember::Aggregate(nested) => {
                    let layout = walk.layout(*nested)?;
                    offset = round_up(offset, layout.align)?;
                    let at = base.checked_add(offset)?;
                    fields.push(self.read_aggregate(walk, *nested, bytes, at)?);
                    offset = offset.checked_add(layout.size)?;
                }
                ForeignMember::Array { element, count } => {
                    let (stride, align) = walk.element_layout(*element)?;
                    offset = round_up(offset, align)?;
                    let at = base.checked_add(offset)?;
                    fields.push(self.read_array(walk, *element, *count, stride, bytes, at)?);
                    offset = offset.checked_add(stride.checked_mul(*count)?)?;
                }
            }
        }
        Some(Value::Struct(self.alloc_struct(fields)))
    }

    /// Reads `count` inline elements at `base` into a Kira array.
    ///
    /// Always exactly `count` elements: C fixed storage has no length of its
    /// own, so what comes back is the whole declared extent, never a prefix
    /// guessed from content.
    fn read_array(
        &mut self,
        walk: &Walk<'_>,
        element: ForeignArrayElement,
        count: u32,
        stride: u32,
        bytes: &[u8],
        base: u32,
    ) -> Option<Value> {
        let mut elements = Vec::with_capacity(count as usize);
        for index in 0..count {
            let at = base.checked_add(stride.checked_mul(index)?)?;
            let value = match element {
                ForeignArrayElement::Scalar(ty) => {
                    let at = at as usize;
                    decode_scalar(ty, bytes.get(at..at + stride as usize)?)?
                }
                ForeignArrayElement::Aggregate(nested) => {
                    self.read_aggregate(walk, nested, bytes, at)?
                }
            };
            elements.push(value);
        }
        Some(Value::Array(self.alloc_array(elements)))
    }
}

impl Heap {
    /// The value a seam writer encodes for `value`: a C block resolves to its
    /// payload address, everything else is already the word it crosses as.
    ///
    /// Resolution reads the block without consuming it — the struct holding
    /// the handle keeps ownership, so the address stays live for as long as
    /// that struct does.
    pub(crate) fn seam_word(&self, value: Value) -> Value {
        match value {
            Value::CBlock(id) => Value::RawPtr(self.cblock_address(id)),
            other => other,
        }
    }
}

/// The little-endian C bytes of one scalar field.
///
/// Every target Kira builds for is little-endian; a big-endian target would
/// need this and [`decode_scalar`] to agree with it, and nothing else.
fn encode_scalar(ty: ForeignType, value: Value, width: ForeignPointerWidth) -> Option<Vec<u8>> {
    Some(match (ty, value) {
        // Narrowing matches what the generated adapter does to a scalar
        // argument, so a field and a parameter of the same C type carry the
        // same value.
        (ForeignType::I8, Value::Int(v)) => (v as i8).to_le_bytes().to_vec(),
        (ForeignType::U8, Value::Int(v)) => (v as u8).to_le_bytes().to_vec(),
        (ForeignType::I16, Value::Int(v)) => (v as i16).to_le_bytes().to_vec(),
        (ForeignType::U16, Value::Int(v)) => (v as u16).to_le_bytes().to_vec(),
        (ForeignType::I32, Value::Int(v)) => (v as i32).to_le_bytes().to_vec(),
        (ForeignType::U32, Value::Int(v)) => (v as u32).to_le_bytes().to_vec(),
        (ForeignType::I64, Value::Int(v)) => v.to_le_bytes().to_vec(),
        (ForeignType::U64, Value::Int(v)) => (v as u64).to_le_bytes().to_vec(),
        (ForeignType::F32, Value::Float(v)) => (v as f32).to_le_bytes().to_vec(),
        (ForeignType::F64, Value::Float(v)) => v.to_le_bytes().to_vec(),
        (ForeignType::Bool, Value::Bool(v)) => vec![c_bool_byte(v)],
        // A `CString` member is a pointer word like any other: the address of
        // storage that outlives the call (see `kira_runtime_abi::c_string`), so
        // it encodes exactly as a `RawPtr` does and null is a zero word.
        (ForeignType::RawPtr | ForeignType::CString, Value::RawPtr(w)) => match width {
            ForeignPointerWidth::Bits32 => (w as u32).to_le_bytes().to_vec(),
            ForeignPointerWidth::Bits64 => w.to_le_bytes().to_vec(),
        },
        // `Void` has no bytes; the frontend refuses it as a member, so it cannot
        // arrive here.
        _ => return None,
    })
}

/// The Kira value of one scalar field read out of its C bytes.
fn decode_scalar(ty: ForeignType, bytes: &[u8]) -> Option<Value> {
    /// Reads a fixed-width array out of a slice of exactly that length.
    fn array<const N: usize>(bytes: &[u8]) -> Option<[u8; N]> {
        bytes.try_into().ok()
    }
    Some(match ty {
        // Widened by the declared signedness, matching how the adapter lifts a
        // scalar result of the same C type.
        ForeignType::I8 => Value::Int(i64::from(i8::from_le_bytes(array(bytes)?))),
        ForeignType::U8 => Value::Int(i64::from(u8::from_le_bytes(array(bytes)?))),
        ForeignType::I16 => Value::Int(i64::from(i16::from_le_bytes(array(bytes)?))),
        ForeignType::U16 => Value::Int(i64::from(u16::from_le_bytes(array(bytes)?))),
        ForeignType::I32 => Value::Int(i64::from(i32::from_le_bytes(array(bytes)?))),
        ForeignType::U32 => Value::Int(i64::from(u32::from_le_bytes(array(bytes)?))),
        ForeignType::I64 => Value::Int(i64::from_le_bytes(array(bytes)?)),
        ForeignType::U64 => Value::Int(u64::from_le_bytes(array(bytes)?) as i64),
        ForeignType::F32 => Value::Float(f64::from(f32::from_le_bytes(array(bytes)?))),
        ForeignType::F64 => Value::Float(f64::from_le_bytes(array(bytes)?)),
        ForeignType::Bool => Value::Bool(bool_from_c_byte(*bytes.first()?)),
        // Read back as the opaque word it is. Kira never dereferences a
        // pointer it did not mint, so a `CString` member read out of C bytes is
        // a `RawPtr` value and nothing more.
        ForeignType::RawPtr | ForeignType::CString => Value::RawPtr(match bytes.len() {
            4 => u64::from(u32::from_le_bytes(array(bytes)?)),
            8 => u64::from_le_bytes(array(bytes)?),
            _ => return None,
        }),
        ForeignType::Void => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use kira_runtime_abi::ForeignAggregate;

    const WIDTH: ForeignPointerWidth = ForeignPointerWidth::Bits64;

    #[test]
    fn a_flat_struct_round_trips_through_its_c_bytes() {
        let mut table = ForeignAggregates::new();
        let id = table
            .push(ForeignAggregate::new(vec![
                ForeignMember::Scalar(ForeignType::F64),
                ForeignMember::Scalar(ForeignType::F64),
            ]))
            .expect("pushes");
        let mut heap = Heap::default();
        let value = Value::Struct(heap.alloc_struct(vec![Value::Float(1.5), Value::Float(2.5)]));

        let bytes = heap
            .aggregate_bytes(&table, id, value, WIDTH)
            .expect("marshals");
        assert_eq!(bytes.len(), 16);
        assert_eq!(&bytes[..8], &1.5f64.to_le_bytes());
        assert_eq!(&bytes[8..], &2.5f64.to_le_bytes());

        let back = heap
            .absorb_aggregate(&table, id, &bytes, WIDTH)
            .expect("rebuilds");
        let Value::Struct(handle) = back else {
            panic!("a struct comes back: {back:?}");
        };
        assert_eq!(heap.fields(handle), [Value::Float(1.5), Value::Float(2.5)]);
    }

    #[test]
    fn padding_bytes_are_written_as_zero_and_skipped_on_the_way_back() {
        // struct { int8_t; double } — seven bytes of padding after the first.
        let mut table = ForeignAggregates::new();
        let id = table
            .push(ForeignAggregate::new(vec![
                ForeignMember::Scalar(ForeignType::I8),
                ForeignMember::Scalar(ForeignType::F64),
            ]))
            .expect("pushes");
        let mut heap = Heap::default();
        let value = Value::Struct(heap.alloc_struct(vec![Value::Int(7), Value::Float(0.25)]));

        let bytes = heap
            .aggregate_bytes(&table, id, value, WIDTH)
            .expect("marshals");
        assert_eq!(bytes.len(), 16);
        assert_eq!(bytes[0], 7);
        assert_eq!(&bytes[1..8], &[0u8; 7], "padding is zero, never stale");
        assert_eq!(&bytes[8..], &0.25f64.to_le_bytes());

        let back = heap
            .absorb_aggregate(&table, id, &bytes, WIDTH)
            .expect("rebuilds");
        let Value::Struct(handle) = back else {
            panic!("a struct comes back");
        };
        assert_eq!(heap.fields(handle), [Value::Int(7), Value::Float(0.25)]);
    }

    #[test]
    fn a_nested_struct_keeps_its_nesting_both_ways() {
        let mut table = ForeignAggregates::new();
        let inner = table
            .push(ForeignAggregate::new(vec![
                ForeignMember::Scalar(ForeignType::I32),
                ForeignMember::Scalar(ForeignType::I32),
            ]))
            .expect("pushes");
        let outer = table
            .push(ForeignAggregate::new(vec![
                ForeignMember::Aggregate(inner),
                ForeignMember::Scalar(ForeignType::F64),
            ]))
            .expect("pushes");
        let mut heap = Heap::default();
        let nested = heap.alloc_struct(vec![Value::Int(3), Value::Int(4)]);
        let value =
            Value::Struct(heap.alloc_struct(vec![Value::Struct(nested), Value::Float(9.0)]));

        let bytes = heap
            .aggregate_bytes(&table, outer, value, WIDTH)
            .expect("marshals");
        // { i32, i32, pad 0, f64 } aligned 8 = 16 bytes.
        assert_eq!(bytes.len(), 16);
        let back = heap
            .absorb_aggregate(&table, outer, &bytes, WIDTH)
            .expect("rebuilds");
        let Value::Struct(handle) = back else {
            panic!("a struct comes back");
        };
        let fields = heap.fields(handle).to_vec();
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[1], Value::Float(9.0));
        let Value::Struct(rebuilt) = fields[0] else {
            panic!("the nested member is a struct: {fields:?}");
        };
        assert_eq!(heap.fields(rebuilt), [Value::Int(3), Value::Int(4)]);
    }

    #[test]
    fn a_value_of_the_wrong_shape_marshals_to_nothing() {
        let mut table = ForeignAggregates::new();
        let id = table
            .push(ForeignAggregate::new(vec![ForeignMember::Scalar(
                ForeignType::I32,
            )]))
            .expect("pushes");
        let mut heap = Heap::default();
        let shape = Err(AggregateMismatch::Shape);
        // Not a struct at all.
        assert_eq!(
            heap.aggregate_bytes(&table, id, Value::Int(1), WIDTH),
            shape
        );
        // A struct with the wrong field count.
        let wrong = Value::Struct(heap.alloc_struct(vec![Value::Int(1), Value::Int(2)]));
        assert_eq!(heap.aggregate_bytes(&table, id, wrong, WIDTH), shape);
        // A field of the wrong kind for its member.
        let bad_kind = Value::Struct(heap.alloc_struct(vec![Value::Float(1.0)]));
        assert_eq!(heap.aggregate_bytes(&table, id, bad_kind, WIDTH), shape);
        // Bytes of the wrong length.
        assert_eq!(heap.absorb_aggregate(&table, id, &[0, 0], WIDTH), None);
    }

    /// A table holding `struct { int32_t[3]; int8_t }` at id 0.
    fn array_table() -> (ForeignAggregates, ForeignAggregateId) {
        let mut table = ForeignAggregates::new();
        let id = table
            .push(ForeignAggregate::new(vec![
                ForeignMember::Array {
                    element: ForeignArrayElement::Scalar(ForeignType::I32),
                    count: 3,
                },
                ForeignMember::Scalar(ForeignType::I8),
            ]))
            .expect("pushes");
        (table, id)
    }

    #[test]
    fn an_inline_array_round_trips_element_for_element() {
        let (table, id) = array_table();
        let mut heap = Heap::default();
        let elements = heap.alloc_array(vec![Value::Int(7), Value::Int(8), Value::Int(9)]);
        let value = Value::Struct(heap.alloc_struct(vec![Value::Array(elements), Value::Int(1)]));

        let bytes = heap
            .aggregate_bytes(&table, id, value, WIDTH)
            .expect("marshals");
        assert_eq!(
            bytes.len(),
            16,
            "12 bytes of array, one byte, three padding"
        );
        assert_eq!(&bytes[0..4], &7i32.to_le_bytes());
        assert_eq!(&bytes[8..12], &9i32.to_le_bytes());
        assert_eq!(bytes[12], 1);

        let back = heap
            .absorb_aggregate(&table, id, &bytes, WIDTH)
            .expect("rebuilds");
        let Value::Struct(handle) = back else {
            panic!("a struct comes back: {back:?}");
        };
        let fields = heap.fields(handle).to_vec();
        let Value::Array(rebuilt) = fields[0] else {
            panic!("the array member comes back as an array: {fields:?}");
        };
        assert_eq!(
            heap.elements(rebuilt),
            [Value::Int(7), Value::Int(8), Value::Int(9)]
        );
    }

    #[test]
    fn a_short_array_leaves_the_rest_of_the_c_storage_zero() {
        let (table, id) = array_table();
        let mut heap = Heap::default();
        let elements = heap.alloc_array(vec![Value::Int(5)]);
        let value = Value::Struct(heap.alloc_struct(vec![Value::Array(elements), Value::Int(0)]));

        let bytes = heap
            .aggregate_bytes(&table, id, value, WIDTH)
            .expect("marshals");
        assert_eq!(&bytes[0..4], &5i32.to_le_bytes());
        assert_eq!(&bytes[4..12], &[0u8; 8], "the unwritten elements are zero");

        // And the whole extent comes back, because C storage carries no length.
        let back = heap
            .absorb_aggregate(&table, id, &bytes, WIDTH)
            .expect("rebuilds");
        let Value::Struct(handle) = back else {
            panic!("a struct comes back");
        };
        let Value::Array(rebuilt) = heap.fields(handle)[0] else {
            panic!("an array comes back");
        };
        assert_eq!(heap.elements(rebuilt).len(), 3);
    }

    #[test]
    fn an_array_longer_than_the_c_extent_is_refused_by_name() {
        let (table, id) = array_table();
        let mut heap = Heap::default();
        let elements = heap.alloc_array(vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
            Value::Int(4),
        ]);
        let value = Value::Struct(heap.alloc_struct(vec![Value::Array(elements), Value::Int(0)]));
        assert_eq!(
            heap.aggregate_bytes(&table, id, value, WIDTH),
            Err(AggregateMismatch::ArrayTooLong { count: 3, len: 4 }),
            "dropping the fourth element would hand C a value the program did not write"
        );
    }

    #[test]
    fn an_array_of_aggregates_keeps_each_elements_nesting() {
        let mut table = ForeignAggregates::new();
        let pair = table
            .push(ForeignAggregate::new(vec![
                ForeignMember::Scalar(ForeignType::I32),
                ForeignMember::Scalar(ForeignType::I32),
            ]))
            .expect("pushes");
        let id = table
            .push(ForeignAggregate::new(vec![ForeignMember::Array {
                element: ForeignArrayElement::Aggregate(pair),
                count: 2,
            }]))
            .expect("pushes");
        let mut heap = Heap::default();
        let first = Value::Struct(heap.alloc_struct(vec![Value::Int(1), Value::Int(2)]));
        let second = Value::Struct(heap.alloc_struct(vec![Value::Int(3), Value::Int(4)]));
        let elements = heap.alloc_array(vec![first, second]);
        let value = Value::Struct(heap.alloc_struct(vec![Value::Array(elements)]));

        let bytes = heap
            .aggregate_bytes(&table, id, value, WIDTH)
            .expect("marshals");
        assert_eq!(bytes.len(), 16);
        assert_eq!(&bytes[8..12], &3i32.to_le_bytes());

        let back = heap
            .absorb_aggregate(&table, id, &bytes, WIDTH)
            .expect("rebuilds");
        let Value::Struct(handle) = back else {
            panic!("a struct comes back");
        };
        let Value::Array(rebuilt) = heap.fields(handle)[0] else {
            panic!("an array comes back");
        };
        let Value::Struct(second) = heap.elements(rebuilt)[1] else {
            panic!("each element is a struct");
        };
        assert_eq!(heap.fields(second), [Value::Int(3), Value::Int(4)]);
    }

    #[test]
    fn a_pointer_member_takes_the_targets_width() {
        let mut table = ForeignAggregates::new();
        let id = table
            .push(ForeignAggregate::new(vec![ForeignMember::Scalar(
                ForeignType::RawPtr,
            )]))
            .expect("pushes");
        let mut heap = Heap::default();
        let value = Value::Struct(heap.alloc_struct(vec![Value::RawPtr(0xdead_beef)]));
        assert_eq!(
            heap.aggregate_bytes(&table, id, value, ForeignPointerWidth::Bits32)
                .expect("marshals")
                .len(),
            4
        );
        assert_eq!(
            heap.aggregate_bytes(&table, id, value, ForeignPointerWidth::Bits64)
                .expect("marshals")
                .len(),
            8
        );
    }
}
