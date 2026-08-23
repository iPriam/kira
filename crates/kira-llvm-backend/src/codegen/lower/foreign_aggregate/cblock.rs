//! Ownership transfer for C blocks embedded in aggregate images.

use super::*;

/// One inline-array ownership move inside a parent C-layout image.
struct ArrayCBlockMove {
    element: ForeignArrayElement,
    count: u32,
    stride: u32,
    array_ty: Type,
    parent: LLVMValueRef,
    base: LLVMValueRef,
}

impl FunctionLowering<'_, '_> {
    /// Whether field `index` of struct type `ty` owns a C-block handle.
    pub(in crate::codegen::lower) fn c_storage_slot(
        &self,
        ty: Type,
        index: usize,
    ) -> Result<bool, LlvmError> {
        let Type::Struct(struct_id) = ty else {
            return Ok(false);
        };
        let def = self
            .codegen
            .program
            .types
            .structs()
            .get(struct_id)
            .ok_or(LlvmError::internal("a C-layout field naming no struct"))?;
        Ok(def.owns_c_storage_at(index as u32))
    }

    /// Moves every owned C-block field in `source` under `parent`.
    ///
    /// `base` is the dynamic byte offset of this aggregate inside the parent's
    /// C-layout payload. Each moved source slot is zeroed before the source is
    /// dropped, leaving the parent as the unique owner.
    pub(super) fn move_cblock_members(
        &mut self,
        id: ForeignAggregateId,
        source: LLVMValueRef,
        ty: Type,
        parent: LLVMValueRef,
        base: LLVMValueRef,
    ) -> Result<(), LlvmError> {
        let members = self
            .codegen
            .program
            .foreign_aggregates
            .get(id)
            .ok_or(LlvmError::internal("an aggregate not in the table"))?
            .members()
            .to_vec();
        let field_types = self.struct_field_types(ty, members.len())?;
        let struct_type = self.codegen.llvm_type(ty)?;
        let mut offset = 0u32;
        for (index, member) in members.iter().enumerate() {
            let field = self
                .codegen
                .field_pointer(struct_type, source, index as u32);
            match member {
                ForeignMember::Scalar(scalar) => {
                    let layout = scalar_layout(*scalar, self.codegen.pointer_width);
                    offset = round_up(offset, layout.align)?;
                    if self.c_storage_slot(ty, index)? {
                        self.move_cblock_slot(
                            field,
                            parent,
                            self.add_cblock_offset(base, offset),
                            layout.size,
                        );
                    }
                    offset = offset
                        .checked_add(layout.size)
                        .ok_or(LlvmError::internal("an aggregate larger than 4GiB"))?;
                }
                ForeignMember::Aggregate(nested) => {
                    let layout = self
                        .codegen
                        .program
                        .foreign_aggregates
                        .layout_of(*nested, self.codegen.pointer_width)
                        .map_err(|_| {
                            LlvmError::internal("an aggregate with no computable C layout")
                        })?;
                    offset = round_up(offset, layout.align)?;
                    let nested_base = self.add_cblock_offset(base, offset);
                    self.move_cblock_members(
                        *nested,
                        field,
                        field_types[index],
                        parent,
                        nested_base,
                    )?;
                    offset = offset
                        .checked_add(layout.size)
                        .ok_or(LlvmError::internal("an aggregate larger than 4GiB"))?;
                }
                ForeignMember::Array { element, count } => {
                    let (stride, align) = self.element_layout(*element)?;
                    offset = round_up(offset, align)?;
                    self.move_cblock_array(
                        field,
                        ArrayCBlockMove {
                            element: *element,
                            count: *count,
                            stride,
                            array_ty: field_types[index],
                            parent,
                            base: self.add_cblock_offset(base, offset),
                        },
                    )?;
                    offset = offset
                        .checked_add(
                            stride
                                .checked_mul(*count)
                                .ok_or(LlvmError::internal("an aggregate larger than 4GiB"))?,
                        )
                        .ok_or(LlvmError::internal("an aggregate larger than 4GiB"))?;
                }
            }
        }
        Ok(())
    }

    /// Moves C blocks out of one inline-array field under `parent`.
    fn move_cblock_array(
        &mut self,
        holder: LLVMValueRef,
        moving: ArrayCBlockMove,
    ) -> Result<(), LlvmError> {
        let element_ty = self.codegen.element_of(moving.array_ty)?;
        if !self.codegen.owns_unique_c_storage(element_ty) {
            return Ok(());
        }
        // SAFETY: `holder` addresses the array handle field.
        let array = unsafe {
            LLVMBuildLoad2(
                self.codegen.builder,
                self.codegen.types.ptr,
                holder,
                c"clayout.array".as_ptr(),
            )
        };
        let len = self.call(
            self.codegen.runtime.array_len,
            &mut [array],
            c"clayout.array.len",
        );
        self.trap_if_longer_than(len, moving.count)?;
        let esize = self.codegen.abi_size(element_ty)?;
        let clone = self.codegen.element_clone(element_ty)?;
        self.emit_index_loop(len, |lowering, index| {
            let slot = lowering.call(
                lowering.codegen.runtime.array_slot_mut,
                &mut [holder, index, esize, clone],
                c"clayout.array.slot",
            );
            let stride_value = lowering.codegen.const_int(i64::from(moving.stride));
            // SAFETY: both operands are i64 values in this live context.
            let element_offset = unsafe {
                let delta = LLVMBuildMul(
                    lowering.codegen.builder,
                    index,
                    stride_value,
                    c"clayout.array.delta".as_ptr(),
                );
                LLVMBuildAdd(
                    lowering.codegen.builder,
                    moving.base,
                    delta,
                    c"clayout.array.offset".as_ptr(),
                )
            };
            match moving.element {
                ForeignArrayElement::Aggregate(nested) => lowering.move_cblock_members(
                    nested,
                    slot,
                    element_ty,
                    moving.parent,
                    element_offset,
                ),
                ForeignArrayElement::Scalar(scalar) => {
                    let width = scalar_layout(scalar, lowering.codegen.pointer_width).size;
                    lowering.move_cblock_slot(slot, moving.parent, element_offset, width);
                    Ok(())
                }
            }
        })
    }

    /// Moves one handle from `slot` into a parent image and zeroes the slot.
    fn move_cblock_slot(
        &mut self,
        slot: LLVMValueRef,
        parent: LLVMValueRef,
        offset: LLVMValueRef,
        width: u32,
    ) {
        // SAFETY: `slot` addresses one i64 C-block handle.
        let child = unsafe {
            LLVMBuildLoad2(
                self.codegen.builder,
                self.codegen.types.i64,
                slot,
                c"clayout.child".as_ptr(),
            )
        };
        let width = self.codegen.const_int(i64::from(width));
        self.call(
            self.codegen.runtime.cblock_attach,
            &mut [parent, offset, width, child],
            c"",
        );
        // SAFETY: zero is the empty C-block handle and `slot` is writable.
        unsafe { LLVMBuildStore(self.codegen.builder, self.codegen.const_int(0), slot) };
    }

    /// Adds one compile-time byte offset to a dynamic parent offset.
    fn add_cblock_offset(&self, base: LLVMValueRef, offset: u32) -> LLVMValueRef {
        if offset == 0 {
            return base;
        }
        // SAFETY: both operands are i64 byte offsets in this live context.
        unsafe {
            LLVMBuildAdd(
                self.codegen.builder,
                base,
                self.codegen.const_int(i64::from(offset)),
                c"clayout.offset".as_ptr(),
            )
        }
    }
}
