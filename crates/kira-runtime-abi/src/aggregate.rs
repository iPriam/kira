//! C-layout aggregates at the foreign seam: the member tree, its layout, and
//! the flattened scalar leaves marshalling walks.
//!
//! # Why the tree, and not a flat list with padding
//!
//! An aggregate is described here as the *nested* member tree the C header
//! declares, never as a flattened list with explicit padding members. The two
//! are not interchangeable at the C ABI: an explicit `char pad[4]` is an
//! INTEGER-class member on x86-64 System V and disqualifies a homogeneous float
//! aggregate on AArch64, so a padded flattening of `struct { float x; double y; }`
//! is passed in different registers than the struct it claims to describe.
//! Kira never classifies aggregates itself — the backend hands the tree to a C
//! compiler, which does — and the tree is what makes that handoff faithful.
//!
//! # The forward-reference invariant
//!
//! A member's [`ForeignAggregateId`] is always strictly less than the id of the
//! aggregate containing it. [`ForeignAggregates::push`] maintains it, decoders
//! validate it, and it buys two things at once: layout is a single forward pass
//! with no memoization, and a cyclic aggregate — which has no C layout — cannot
//! be expressed at all.

use crate::foreign::ForeignType;

/// An index into a program's [`ForeignAggregates`] table.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ForeignAggregateId(pub u32);

/// The pointer width of the target a C layout is computed for.
///
/// Layout is target-dependent through exactly one type: a C pointer is four
/// bytes on `wasm32` and eight on every host target Kira builds for. Every
/// other seam scalar has the same size and alignment everywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ForeignPointerWidth {
    /// A 32-bit target, such as `wasm32`.
    Bits32,
    /// A 64-bit target.
    Bits64,
}

impl ForeignPointerWidth {
    /// The width of the target this code is running on.
    ///
    /// Every side that marshals an aggregate at run time — the VM, the dynamic
    /// FFI host, the hybrid native half — lays it out for the machine executing
    /// the call, so all three read this one constant rather than each deciding
    /// what "the host" means.
    pub const HOST: Self = if size_of::<usize>() == 8 {
        Self::Bits64
    } else {
        Self::Bits32
    };

    /// The size in bytes of a pointer on this target.
    pub const fn bytes(self) -> u32 {
        match self {
            Self::Bits32 => 4,
            Self::Bits64 => 8,
        }
    }
}

/// What an inline fixed-size array member holds, repeated `count` times.
///
/// An element is a scalar or a nested aggregate, never another array: a C array
/// of arrays is described as an array of the one-member aggregate wrapping the
/// inner array, which has the same layout and keeps this type flat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ForeignArrayElement {
    /// A seam scalar element.
    Scalar(ForeignType),
    /// A nested aggregate element, by table index.
    Aggregate(ForeignAggregateId),
}

/// One member of a C-layout aggregate, in declaration order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ForeignMember {
    /// A seam scalar member.
    Scalar(ForeignType),
    /// A nested aggregate member, by table index.
    Aggregate(ForeignAggregateId),
    /// An inline fixed-size C array member — `E name[count]`.
    ///
    /// Inline storage, not a pointer: the elements live inside the containing
    /// aggregate's bytes, so the member's size is `count` times the element's.
    Array {
        /// What each element is.
        element: ForeignArrayElement,
        /// How many elements the C declaration reserves. Never zero — a
        /// zero-length array is not standard C, and the frontend refuses one.
        count: u32,
    },
}

/// A C-layout aggregate: its members in declaration order.
///
/// Carries no name. The generated shim redeclares each aggregate structurally
/// under a name Kira mints, so the C tag name in the original header is not part
/// of the contract.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ForeignAggregate {
    members: Box<[ForeignMember]>,
}

impl ForeignAggregate {
    /// Creates an aggregate from its members in declaration order.
    pub fn new(members: impl Into<Box<[ForeignMember]>>) -> Self {
        Self {
            members: members.into(),
        }
    }

    /// Returns the members in declaration order.
    pub fn members(&self) -> &[ForeignMember] {
        &self.members
    }
}

/// The size and alignment of one aggregate on a given target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ForeignLayout {
    /// The aggregate's `sizeof`, a multiple of [`ForeignLayout::align`].
    pub size: u32,
    /// The aggregate's `_Alignof`, always a power of two.
    pub align: u32,
}

/// One scalar leaf of an aggregate, at its byte offset from the aggregate's
/// start.
///
/// Leaves come in depth-first declaration order, which is the order a Kira
/// struct value's fields are walked, so marshalling pairs the two by position
/// without carrying a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ForeignLeaf {
    /// The byte offset of this scalar within the outermost aggregate.
    pub offset: u32,
    /// The scalar's seam type.
    pub ty: ForeignType,
}

/// Why an aggregate table could not be used as given.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ForeignAggregateError {
    /// A member names an aggregate id that is not below the containing one.
    ///
    /// Either the id is out of range, or it is a forward or self reference —
    /// which would describe an aggregate containing itself, and so having no C
    /// layout.
    #[error(
        "aggregate {container} has a member referring to aggregate {member}, which is not a lower index"
    )]
    ForwardReference {
        /// The containing aggregate's index.
        container: u32,
        /// The referenced index.
        member: u32,
    },
    /// An aggregate's computed size exceeded the 32-bit byte count the seam
    /// carries.
    #[error("aggregate {index} is too large to describe at the seam")]
    TooLarge {
        /// The offending aggregate's index.
        index: u32,
    },
    /// The table names an aggregate index it does not contain.
    #[error("no aggregate {0} in this table")]
    UnknownAggregate(u32),
}

/// A program's C-layout aggregate table.
///
/// Every aggregate any `@FFI.Extern` signature names appears here exactly once,
/// nested ones before the aggregates that contain them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ForeignAggregates {
    entries: Vec<ForeignAggregate>,
}

impl ForeignAggregates {
    /// Creates an empty table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends an aggregate, returning its id.
    ///
    /// Returns [`ForeignAggregateError::ForwardReference`] when a member names
    /// an id this table does not already hold — the invariant that makes layout
    /// a forward pass and a cycle unrepresentable.
    pub fn push(
        &mut self,
        aggregate: ForeignAggregate,
    ) -> Result<ForeignAggregateId, ForeignAggregateError> {
        let index = self.entries.len() as u32;
        for member in aggregate.members() {
            let referenced = match member {
                ForeignMember::Scalar(_) => None,
                ForeignMember::Aggregate(id) => Some(id.0),
                ForeignMember::Array { element, .. } => match element {
                    ForeignArrayElement::Scalar(_) => None,
                    ForeignArrayElement::Aggregate(id) => Some(id.0),
                },
            };
            if let Some(referenced) = referenced
                && referenced >= index
            {
                return Err(ForeignAggregateError::ForwardReference {
                    container: index,
                    member: referenced,
                });
            }
        }
        self.entries.push(aggregate);
        Ok(ForeignAggregateId(index))
    }

    /// Returns the aggregate at `id`, or `None` when the table has no such row.
    pub fn get(&self, id: ForeignAggregateId) -> Option<&ForeignAggregate> {
        self.entries.get(id.0 as usize)
    }

    /// Returns the number of aggregates in the table.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns whether the table is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns the aggregates in table order, lowest id first.
    pub fn iter(&self) -> impl Iterator<Item = &ForeignAggregate> {
        self.entries.iter()
    }

    /// Computes the layout of every aggregate for `width`, in table order.
    ///
    /// One forward pass: an aggregate's members are all at lower indices, so
    /// their layouts are already known when it is reached.
    pub fn layouts(
        &self,
        width: ForeignPointerWidth,
    ) -> Result<Vec<ForeignLayout>, ForeignAggregateError> {
        let mut layouts: Vec<ForeignLayout> = Vec::with_capacity(self.entries.len());
        for (index, aggregate) in self.entries.iter().enumerate() {
            let index = index as u32;
            let mut size: u32 = 0;
            let mut align: u32 = 1;
            for member in aggregate.members() {
                let member_layout = member_layout(member, index, width, &layouts)?;
                align = align.max(member_layout.align);
                size = round_up(size, member_layout.align)
                    .and_then(|start| start.checked_add(member_layout.size))
                    .ok_or(ForeignAggregateError::TooLarge { index })?;
            }
            // An empty C struct is not standard C, but a Kira struct with no
            // fields is expressible; C compilers give it size 1 as an extension,
            // and that is what the generated shim would produce.
            let size =
                round_up(size.max(1), align).ok_or(ForeignAggregateError::TooLarge { index })?;
            layouts.push(ForeignLayout { size, align });
        }
        Ok(layouts)
    }

    /// Returns the layout of one aggregate for `width`.
    pub fn layout_of(
        &self,
        id: ForeignAggregateId,
        width: ForeignPointerWidth,
    ) -> Result<ForeignLayout, ForeignAggregateError> {
        self.layouts(width)?
            .get(id.0 as usize)
            .copied()
            .ok_or(ForeignAggregateError::UnknownAggregate(id.0))
    }

    /// Returns the scalar leaves of one aggregate in depth-first declaration
    /// order, each at its byte offset from the aggregate's start.
    pub fn leaves_of(
        &self,
        id: ForeignAggregateId,
        width: ForeignPointerWidth,
    ) -> Result<Vec<ForeignLeaf>, ForeignAggregateError> {
        let layouts = self.layouts(width)?;
        let mut leaves = Vec::new();
        self.collect_leaves(id, width, &layouts, 0, &mut leaves)?;
        Ok(leaves)
    }

    /// Returns the byte offset of each **top-level** member of one aggregate,
    /// in declaration order.
    ///
    /// [`Self::leaves_of`] flattens: a nested aggregate contributes one leaf per
    /// scalar inside it, so a member's index is not its leaf's index. Reading a
    /// named field through a pointer needs the member's own offset, which is
    /// what this returns — one entry per member, whatever that member is.
    pub fn member_offsets_of(
        &self,
        id: ForeignAggregateId,
        width: ForeignPointerWidth,
    ) -> Result<Vec<u32>, ForeignAggregateError> {
        let layouts = self.layouts(width)?;
        let aggregate = self
            .get(id)
            .ok_or(ForeignAggregateError::UnknownAggregate(id.0))?;
        let mut offsets = Vec::with_capacity(aggregate.members().len());
        let mut offset: u32 = 0;
        for member in aggregate.members() {
            let layout = member_layout(member, id.0, width, &layouts)?;
            offset = round_up(offset, layout.align)
                .ok_or(ForeignAggregateError::TooLarge { index: id.0 })?;
            offsets.push(offset);
            offset = offset
                .checked_add(layout.size)
                .ok_or(ForeignAggregateError::TooLarge { index: id.0 })?;
        }
        Ok(offsets)
    }

    /// Appends `id`'s leaves, shifted by `base`, to `leaves`.
    fn collect_leaves(
        &self,
        id: ForeignAggregateId,
        width: ForeignPointerWidth,
        layouts: &[ForeignLayout],
        base: u32,
        leaves: &mut Vec<ForeignLeaf>,
    ) -> Result<(), ForeignAggregateError> {
        let aggregate = self
            .get(id)
            .ok_or(ForeignAggregateError::UnknownAggregate(id.0))?;
        let mut offset: u32 = 0;
        for member in aggregate.members() {
            match member {
                ForeignMember::Scalar(ty) => {
                    let layout = scalar_layout(*ty, width);
                    offset = round_up(offset, layout.align)
                        .ok_or(ForeignAggregateError::TooLarge { index: id.0 })?;
                    leaves.push(ForeignLeaf {
                        offset: base
                            .checked_add(offset)
                            .ok_or(ForeignAggregateError::TooLarge { index: id.0 })?,
                        ty: *ty,
                    });
                    offset = offset
                        .checked_add(layout.size)
                        .ok_or(ForeignAggregateError::TooLarge { index: id.0 })?;
                }
                ForeignMember::Aggregate(nested) => {
                    let layout = *layouts.get(nested.0 as usize).ok_or(
                        ForeignAggregateError::ForwardReference {
                            container: id.0,
                            member: nested.0,
                        },
                    )?;
                    offset = round_up(offset, layout.align)
                        .ok_or(ForeignAggregateError::TooLarge { index: id.0 })?;
                    let nested_base = base
                        .checked_add(offset)
                        .ok_or(ForeignAggregateError::TooLarge { index: id.0 })?;
                    self.collect_leaves(*nested, width, layouts, nested_base, leaves)?;
                    offset = offset
                        .checked_add(layout.size)
                        .ok_or(ForeignAggregateError::TooLarge { index: id.0 })?;
                }
                ForeignMember::Array { element, count } => {
                    let whole = member_layout(member, id.0, width, layouts)?;
                    let stride = element_layout(*element, id.0, width, layouts)?.size;
                    offset = round_up(offset, whole.align)
                        .ok_or(ForeignAggregateError::TooLarge { index: id.0 })?;
                    // Each element is its own leaf, or its own subtree of them,
                    // at the element's own stride from the array's start.
                    for step in 0..*count {
                        let at = base
                            .checked_add(offset)
                            .and_then(|start| start.checked_add(stride.checked_mul(step)?))
                            .ok_or(ForeignAggregateError::TooLarge { index: id.0 })?;
                        match element {
                            ForeignArrayElement::Scalar(ty) => {
                                leaves.push(ForeignLeaf {
                                    offset: at,
                                    ty: *ty,
                                });
                            }
                            ForeignArrayElement::Aggregate(nested) => {
                                self.collect_leaves(*nested, width, layouts, at, leaves)?;
                            }
                        }
                    }
                    offset = offset
                        .checked_add(whole.size)
                        .ok_or(ForeignAggregateError::TooLarge { index: id.0 })?;
                }
            }
        }
        Ok(())
    }
}

/// The layout of one inline-array element.
fn element_layout(
    element: ForeignArrayElement,
    container: u32,
    width: ForeignPointerWidth,
    layouts: &[ForeignLayout],
) -> Result<ForeignLayout, ForeignAggregateError> {
    match element {
        ForeignArrayElement::Scalar(ty) => Ok(scalar_layout(ty, width)),
        ForeignArrayElement::Aggregate(id) => {
            layouts
                .get(id.0 as usize)
                .copied()
                .ok_or(ForeignAggregateError::ForwardReference {
                    container,
                    member: id.0,
                })
        }
    }
}

/// The layout one member occupies inside the aggregate at `container`.
///
/// Every aggregate a member can name is at a lower index, so `layouts` already
/// holds it — a reference it does not hold is the forward reference the table's
/// invariant forbids, reported rather than assumed away.
fn member_layout(
    member: &ForeignMember,
    container: u32,
    width: ForeignPointerWidth,
    layouts: &[ForeignLayout],
) -> Result<ForeignLayout, ForeignAggregateError> {
    match member {
        ForeignMember::Scalar(ty) => Ok(scalar_layout(*ty, width)),
        ForeignMember::Aggregate(id) => {
            layouts
                .get(id.0 as usize)
                .copied()
                .ok_or(ForeignAggregateError::ForwardReference {
                    container,
                    member: id.0,
                })
        }
        ForeignMember::Array { element, count } => {
            let element = element_layout(*element, container, width, layouts)?;
            // C sizes an array as `count * sizeof(element)`, with nothing
            // between elements: an element's own size is already a multiple of
            // its alignment, so the stride is the size.
            let size = element
                .size
                .checked_mul(*count)
                .ok_or(ForeignAggregateError::TooLarge { index: container })?;
            Ok(ForeignLayout {
                size,
                align: element.align,
            })
        }
    }
}

/// The size and alignment of a seam scalar in C on `width`.
///
/// Every scalar but a pointer is fixed-width by construction — that is what the
/// seam's refusal of bare `Int`/`Float` buys — and C `_Bool` is one byte on
/// every target Kira builds for.
pub const fn scalar_layout(ty: ForeignType, width: ForeignPointerWidth) -> ForeignLayout {
    let size = match ty {
        ForeignType::Void => 0,
        ForeignType::I8 | ForeignType::U8 | ForeignType::Bool => 1,
        ForeignType::I16 | ForeignType::U16 => 2,
        ForeignType::I32 | ForeignType::U32 | ForeignType::F32 => 4,
        ForeignType::I64 | ForeignType::U64 | ForeignType::F64 => 8,
        ForeignType::RawPtr | ForeignType::CString => width.bytes(),
    };
    ForeignLayout {
        size,
        align: if size == 0 { 1 } else { size },
    }
}

/// Rounds `value` up to the next multiple of `align`, or `None` on overflow.
const fn round_up(value: u32, align: u32) -> Option<u32> {
    match value.checked_add(align - 1) {
        Some(raised) => Some(raised - (raised % align)),
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_inline_array_member_is_sized_by_its_element_and_extent() {
        // struct { int8_t; int32_t[3] } — one byte, three of padding, twelve.
        let mut table = ForeignAggregates::new();
        let id = table
            .push(ForeignAggregate::new(vec![
                ForeignMember::Scalar(ForeignType::I8),
                ForeignMember::Array {
                    element: ForeignArrayElement::Scalar(ForeignType::I32),
                    count: 3,
                },
            ]))
            .expect("pushes");
        let layout = table
            .layout_of(id, ForeignPointerWidth::Bits64)
            .expect("layout");
        assert_eq!(layout, ForeignLayout { size: 16, align: 4 });

        let leaves = table
            .leaves_of(id, ForeignPointerWidth::Bits64)
            .expect("leaves");
        let offsets: Vec<u32> = leaves.iter().map(|leaf| leaf.offset).collect();
        assert_eq!(offsets, [0, 4, 8, 12], "each element is its own leaf");
    }

    #[test]
    fn an_array_of_aggregates_expands_each_elements_leaves() {
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
        assert_eq!(
            table
                .layout_of(id, ForeignPointerWidth::Bits64)
                .expect("layout"),
            ForeignLayout { size: 16, align: 4 }
        );
        let offsets: Vec<u32> = table
            .leaves_of(id, ForeignPointerWidth::Bits64)
            .expect("leaves")
            .iter()
            .map(|leaf| leaf.offset)
            .collect();
        assert_eq!(offsets, [0, 4, 8, 12]);
    }

    #[test]
    fn an_array_element_may_not_name_a_forward_aggregate() {
        let mut table = ForeignAggregates::new();
        // Nothing is in the table yet, so element id 0 is this very row.
        let refused = table.push(ForeignAggregate::new(vec![ForeignMember::Array {
            element: ForeignArrayElement::Aggregate(ForeignAggregateId(0)),
            count: 2,
        }]));
        assert_eq!(
            refused,
            Err(ForeignAggregateError::ForwardReference {
                container: 0,
                member: 0
            })
        );
    }

    /// `struct { i32; i32; i32 }` — three words, no padding.
    fn flat_i32s() -> ForeignAggregate {
        ForeignAggregate::new(vec![ForeignMember::Scalar(ForeignType::I32); 3])
    }

    #[test]
    fn a_flat_aggregate_packs_without_padding() {
        let mut table = ForeignAggregates::new();
        let id = table.push(flat_i32s()).expect("pushes");
        assert_eq!(
            table.layout_of(id, ForeignPointerWidth::Bits64),
            Ok(ForeignLayout { size: 12, align: 4 })
        );
        assert_eq!(
            table.leaves_of(id, ForeignPointerWidth::Bits64),
            Ok(vec![
                ForeignLeaf {
                    offset: 0,
                    ty: ForeignType::I32
                },
                ForeignLeaf {
                    offset: 4,
                    ty: ForeignType::I32
                },
                ForeignLeaf {
                    offset: 8,
                    ty: ForeignType::I32
                },
            ])
        );
    }

    #[test]
    fn a_mixed_aggregate_pads_to_member_alignment_and_rounds_its_size() {
        // struct { char c; double d; char e; } is 1 + 7 pad + 8 + 1 + 7 pad.
        let mut table = ForeignAggregates::new();
        let id = table
            .push(ForeignAggregate::new(vec![
                ForeignMember::Scalar(ForeignType::I8),
                ForeignMember::Scalar(ForeignType::F64),
                ForeignMember::Scalar(ForeignType::I8),
            ]))
            .expect("pushes");
        assert_eq!(
            table.layout_of(id, ForeignPointerWidth::Bits64),
            Ok(ForeignLayout { size: 24, align: 8 })
        );
        let leaves = table
            .leaves_of(id, ForeignPointerWidth::Bits64)
            .expect("leaves");
        assert_eq!(
            leaves.iter().map(|leaf| leaf.offset).collect::<Vec<_>>(),
            vec![0, 8, 16]
        );
    }

    #[test]
    fn a_nested_aggregate_contributes_its_own_alignment_and_flattens_in_order() {
        let mut table = ForeignAggregates::new();
        let inner = table
            .push(ForeignAggregate::new(vec![
                ForeignMember::Scalar(ForeignType::I16),
                ForeignMember::Scalar(ForeignType::F64),
            ]))
            .expect("pushes");
        let outer = table
            .push(ForeignAggregate::new(vec![
                ForeignMember::Scalar(ForeignType::I8),
                ForeignMember::Aggregate(inner),
                ForeignMember::Scalar(ForeignType::I32),
            ]))
            .expect("pushes");
        // inner is { i16, pad 6, f64 } = 16 bytes aligned 8.
        assert_eq!(
            table.layout_of(inner, ForeignPointerWidth::Bits64),
            Ok(ForeignLayout { size: 16, align: 8 })
        );
        // outer is { i8, pad 7, inner(16), i32, pad 4 } = 32 bytes aligned 8.
        assert_eq!(
            table.layout_of(outer, ForeignPointerWidth::Bits64),
            Ok(ForeignLayout { size: 32, align: 8 })
        );
        assert_eq!(
            table.leaves_of(outer, ForeignPointerWidth::Bits64),
            Ok(vec![
                ForeignLeaf {
                    offset: 0,
                    ty: ForeignType::I8
                },
                ForeignLeaf {
                    offset: 8,
                    ty: ForeignType::I16
                },
                ForeignLeaf {
                    offset: 16,
                    ty: ForeignType::F64
                },
                ForeignLeaf {
                    offset: 24,
                    ty: ForeignType::I32
                },
            ])
        );
    }

    #[test]
    fn a_pointer_member_takes_the_targets_width() {
        let mut table = ForeignAggregates::new();
        let id = table
            .push(ForeignAggregate::new(vec![
                ForeignMember::Scalar(ForeignType::I32),
                ForeignMember::Scalar(ForeignType::RawPtr),
            ]))
            .expect("pushes");
        assert_eq!(
            table.layout_of(id, ForeignPointerWidth::Bits64),
            Ok(ForeignLayout { size: 16, align: 8 })
        );
        assert_eq!(
            table.layout_of(id, ForeignPointerWidth::Bits32),
            Ok(ForeignLayout { size: 8, align: 4 })
        );
    }

    #[test]
    fn a_member_that_is_not_a_lower_index_is_refused() {
        let mut table = ForeignAggregates::new();
        assert_eq!(
            table.push(ForeignAggregate::new(vec![ForeignMember::Aggregate(
                ForeignAggregateId(0)
            )])),
            Err(ForeignAggregateError::ForwardReference {
                container: 0,
                member: 0
            })
        );
        table.push(flat_i32s()).expect("pushes");
        assert_eq!(
            table.push(ForeignAggregate::new(vec![ForeignMember::Aggregate(
                ForeignAggregateId(5)
            )])),
            Err(ForeignAggregateError::ForwardReference {
                container: 1,
                member: 5
            })
        );
    }

    #[test]
    fn an_empty_aggregate_has_the_one_byte_c_extension_size() {
        let mut table = ForeignAggregates::new();
        let id = table
            .push(ForeignAggregate::new(Vec::new()))
            .expect("pushes");
        assert_eq!(
            table.layout_of(id, ForeignPointerWidth::Bits64),
            Ok(ForeignLayout { size: 1, align: 1 })
        );
        assert_eq!(table.leaves_of(id, ForeignPointerWidth::Bits64), Ok(vec![]));
    }

    /// A member's offset is its own, not its first leaf's.
    ///
    /// `{ i32, { i32, i32 }, i32 }` has three members and four leaves, so the
    /// last member is leaf 3 — reading it by leaf index would read the wrong
    /// field.
    #[test]
    fn member_offsets_are_per_member_not_per_leaf() {
        let mut table = ForeignAggregates::new();
        let inner = table
            .push(ForeignAggregate::new(vec![
                ForeignMember::Scalar(ForeignType::I32),
                ForeignMember::Scalar(ForeignType::I32),
            ]))
            .expect("inner");
        let outer = table
            .push(ForeignAggregate::new(vec![
                ForeignMember::Scalar(ForeignType::I32),
                ForeignMember::Aggregate(inner),
                ForeignMember::Scalar(ForeignType::I32),
            ]))
            .expect("outer");
        let width = ForeignPointerWidth::Bits64;
        assert_eq!(
            table.member_offsets_of(outer, width).expect("offsets"),
            [0, 4, 12]
        );
        let leaves: Vec<u32> = table
            .leaves_of(outer, width)
            .expect("leaves")
            .iter()
            .map(|leaf| leaf.offset)
            .collect();
        assert_eq!(leaves, [0, 4, 8, 12], "four leaves, three members");
    }

    /// A member after a wider one starts at its own alignment, not packed.
    #[test]
    fn member_offsets_respect_alignment_padding() {
        let mut table = ForeignAggregates::new();
        let id = table
            .push(ForeignAggregate::new(vec![
                ForeignMember::Scalar(ForeignType::I8),
                ForeignMember::Scalar(ForeignType::I64),
                ForeignMember::Scalar(ForeignType::I8),
            ]))
            .expect("aggregate");
        assert_eq!(
            table
                .member_offsets_of(id, ForeignPointerWidth::Bits64)
                .expect("offsets"),
            [0, 8, 16]
        );
    }
}
