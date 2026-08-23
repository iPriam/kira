//! Ownership transfer for C blocks embedded in aggregate images.

use super::*;

/// One inline-array ownership move inside a parent C-layout image.
struct ArrayCBlockMove {
    element: ForeignArrayElement,
    count: u32,
    stride: u32,
    base: u32,
    root: CBlockId,
}

impl Heap {
    /// Builds an owned C-layout image and moves every embedded block under it.
    pub fn cblock_aggregate_image(
        &mut self,
        table: &ForeignAggregates,
        id: ForeignAggregateId,
        value: Value,
        width: ForeignPointerWidth,
        bytes: Vec<u8>,
    ) -> Result<CBlockId, AggregateMismatch> {
        let walk = Walk::new(table, width).ok_or(AggregateMismatch::Shape)?;
        let root = self.cblock_bytes(bytes);
        let moved = self.move_aggregate_cblocks(&walk, id, value, 0, root);
        self.drop_value(value);
        if let Err(error) = moved {
            self.free_cblock(root);
            return Err(error);
        }
        Ok(root)
    }

    /// Moves C blocks out of one aggregate value and under `root`.
    fn move_aggregate_cblocks(
        &mut self,
        walk: &Walk<'_>,
        id: ForeignAggregateId,
        value: Value,
        base: u32,
        root: CBlockId,
    ) -> Result<(), AggregateMismatch> {
        let aggregate = walk.table.get(id).ok_or(AggregateMismatch::Shape)?;
        let Value::Struct(struct_id) = value else {
            return Err(AggregateMismatch::Shape);
        };
        self.make_struct_unique(struct_id);
        if self.fields(struct_id).len() != aggregate.members().len() {
            return Err(AggregateMismatch::Shape);
        }
        let mut offset = 0u32;
        for (index, member) in aggregate.members().iter().enumerate() {
            let field = self
                .field(struct_id, index as u64)
                .ok_or(AggregateMismatch::Shape)?;
            match member {
                ForeignMember::Scalar(scalar) => {
                    let layout = scalar_layout(*scalar, walk.width);
                    offset = shape(round_up(offset, layout.align))?;
                    if let Value::CBlock(child) = field {
                        self.take_struct_cblock(struct_id, index, child)?;
                        self.attach_moved_cblock(
                            root,
                            base.checked_add(offset).ok_or(AggregateMismatch::Shape)?,
                            walk.width,
                            child,
                        )?;
                    }
                    offset = shape(offset.checked_add(layout.size))?;
                }
                ForeignMember::Aggregate(nested) => {
                    let layout = shape(walk.layout(*nested))?;
                    offset = shape(round_up(offset, layout.align))?;
                    self.move_aggregate_cblocks(
                        walk,
                        *nested,
                        field,
                        shape(base.checked_add(offset))?,
                        root,
                    )?;
                    offset = shape(offset.checked_add(layout.size))?;
                }
                ForeignMember::Array { element, count } => {
                    let (stride, align) = shape(walk.element_layout(*element))?;
                    offset = shape(round_up(offset, align))?;
                    self.move_array_cblocks(
                        walk,
                        field,
                        ArrayCBlockMove {
                            element: *element,
                            count: *count,
                            stride,
                            base: shape(base.checked_add(offset))?,
                            root,
                        },
                    )?;
                    offset = shape(offset.checked_add(shape(stride.checked_mul(*count))?))?;
                }
            }
        }
        Ok(())
    }

    /// Moves C blocks out of one inline-array field and under `root`.
    fn move_array_cblocks(
        &mut self,
        walk: &Walk<'_>,
        value: Value,
        moving: ArrayCBlockMove,
    ) -> Result<(), AggregateMismatch> {
        let Value::Array(array_id) = value else {
            return Err(AggregateMismatch::Shape);
        };
        self.make_array_unique(array_id);
        let len = self.array_len(array_id).ok_or(AggregateMismatch::Shape)?;
        if len > moving.count as usize {
            return Err(AggregateMismatch::ArrayTooLong {
                count: moving.count,
                len,
            });
        }
        for index in 0..len {
            let value = self
                .element(array_id, index)
                .ok_or(AggregateMismatch::Shape)?;
            let at = moving
                .base
                .checked_add(
                    moving
                        .stride
                        .checked_mul(index as u32)
                        .ok_or(AggregateMismatch::Shape)?,
                )
                .ok_or(AggregateMismatch::Shape)?;
            match moving.element {
                ForeignArrayElement::Aggregate(nested) => {
                    self.move_aggregate_cblocks(walk, nested, value, at, moving.root)?;
                }
                ForeignArrayElement::Scalar(_) => {
                    if let Value::CBlock(child) = value {
                        self.take_array_cblock(array_id, index, child)?;
                        self.attach_moved_cblock(moving.root, at, walk.width, child)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Replaces one struct field's moved C block with null.
    fn take_struct_cblock(
        &mut self,
        id: StructId,
        index: usize,
        child: CBlockId,
    ) -> Result<(), AggregateMismatch> {
        let Some(Some(Object::Struct(fields))) = self.slots.get_mut(id.0 as usize) else {
            return Err(AggregateMismatch::Shape);
        };
        let Some(fields) = std::rc::Rc::get_mut(fields) else {
            return Err(AggregateMismatch::Shape);
        };
        let Some(slot) = fields.get_mut(index) else {
            return Err(AggregateMismatch::Shape);
        };
        if *slot != Value::CBlock(child) {
            return Err(AggregateMismatch::Shape);
        }
        *slot = Value::RawPtr(0);
        Ok(())
    }

    /// Replaces one array element's moved C block with null.
    fn take_array_cblock(
        &mut self,
        id: ArrayId,
        index: usize,
        child: CBlockId,
    ) -> Result<(), AggregateMismatch> {
        let Some(Some(Object::Array(elements))) = self.slots.get_mut(id.0 as usize) else {
            return Err(AggregateMismatch::Shape);
        };
        let Some(elements) = std::rc::Rc::get_mut(elements) else {
            return Err(AggregateMismatch::Shape);
        };
        let Some(slot) = elements.get_mut(index) else {
            return Err(AggregateMismatch::Shape);
        };
        if *slot != Value::CBlock(child) {
            return Err(AggregateMismatch::Shape);
        }
        *slot = Value::RawPtr(0);
        Ok(())
    }

    /// Attaches a moved child or frees it if a malformed layout rejects it.
    fn attach_moved_cblock(
        &mut self,
        root: CBlockId,
        offset: u32,
        width: ForeignPointerWidth,
        child: CBlockId,
    ) -> Result<(), AggregateMismatch> {
        if self.cblock_attach(
            root,
            kira_runtime_abi::CBlockOffset::new(u64::from(offset)),
            width,
            child,
        ) {
            return Ok(());
        }
        self.free_cblock(child);
        Err(AggregateMismatch::Shape)
    }
}
