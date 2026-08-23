//! Marshalling a Kira struct value into C-layout bytes and back, in native code.
//!
//! The native mirror of the VM's `value::aggregate` walk, and it has to produce
//! byte-identical buffers: the same program on `kira run --backend vm` and
//! `--backend llvm` hands the same generated adapter the same bytes, or the two
//! backends disagree about what a C function was called with.
//!
//! # Why the tree, and not a flat leaf list
//!
//! A Kira struct is an LLVM aggregate value reached by `extractvalue` at each
//! level, so writing it out needs the *path* to each scalar, not just the byte
//! offset of each leaf. The table's member tree carries that structure; a
//! flattened leaf list would not. Both walks recurse over the tree with the
//! current LLVM value and the current base offset.
//!
//! # Padding
//!
//! The buffer is zeroed before any field is written. C leaves padding
//! unspecified, but a foreign call that hands over uninitialized stack bytes is
//! a call whose result can depend on what the frame held a moment ago — and on
//! whether the VM or the native backend made it. Zeroing costs one `memset` and
//! makes the two engines agree byte for byte.

use kira_runtime_abi::{ForeignAggregateId, ForeignArrayElement, ForeignMember, scalar_layout};
use kira_semantics_model::Type;
use llvm_sys::core::*;
use llvm_sys::prelude::*;

use super::FunctionLowering;
use crate::LlvmError;

mod cblock;

impl FunctionLowering<'_, '_> {
    /// Writes the Kira struct `value` of Kira type `ty` into a fresh C-layout
    /// buffer.
    pub(super) fn write_aggregate_buffer(
        &mut self,
        id: ForeignAggregateId,
        value: LLVMValueRef,
        ty: Type,
    ) -> Result<LLVMValueRef, LlvmError> {
        let buffer = self.aggregate_alloca(id)?;
        let layout = self
            .codegen
            .program
            .foreign_aggregates
            .layout_of(id, self.codegen.pointer_width)
            .map_err(|_| LlvmError::internal("an aggregate with no computable C layout"))?;
        let types = self.codegen.types;
        let builder = self.codegen.builder;
        // SAFETY: `buffer` is a live alloca of exactly `layout.size` bytes on
        // this block, and the builder is positioned on it.
        unsafe {
            LLVMBuildMemSet(
                builder,
                buffer,
                LLVMConstInt(types.i8, 0, 0),
                LLVMConstInt(types.i64, u64::from(layout.size), 0),
                layout.align,
            );
        }
        self.write_members(id, value, ty, buffer, 0)?;
        Ok(buffer)
    }

    /// Writes one aggregate's members into `buffer` at `base`.
    fn write_members(
        &mut self,
        id: ForeignAggregateId,
        value: LLVMValueRef,
        ty: Type,
        buffer: LLVMValueRef,
        base: u32,
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
        let mut offset = 0u32;
        for (index, member) in members.iter().enumerate() {
            let field = self.extract_field(value, index as u32)?;
            match member {
                ForeignMember::Scalar(scalar) => {
                    let layout = scalar_layout(*scalar, self.codegen.pointer_width);
                    offset = round_up(offset, layout.align)?;
                    let at = base
                        .checked_add(offset)
                        .ok_or(LlvmError::internal("an aggregate offset past 4GiB"))?;
                    let slot = self.byte_offset_ptr(buffer, at)?;
                    let field = if self.c_storage_slot(ty, index)? {
                        self.call(
                            self.codegen.runtime.cblock_word,
                            &mut [field],
                            c"aggregate.cblock.word",
                        )
                    } else {
                        field
                    };
                    let converted = self.codegen.kira_value_to_c(field, *scalar)?;
                    // SAFETY: `slot` addresses `layout.size` bytes inside the
                    // buffer, and `converted` has exactly this scalar's C type.
                    unsafe {
                        let store = LLVMBuildStore(self.codegen.builder, converted, slot);
                        LLVMSetAlignment(store, layout.align);
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
                    let at = base
                        .checked_add(offset)
                        .ok_or(LlvmError::internal("an aggregate offset past 4GiB"))?;
                    self.write_members(*nested, field, field_types[index], buffer, at)?;
                    offset = offset
                        .checked_add(layout.size)
                        .ok_or(LlvmError::internal("an aggregate larger than 4GiB"))?;
                }
                ForeignMember::Array { element, count } => {
                    let (stride, align) = self.element_layout(*element)?;
                    offset = round_up(offset, align)?;
                    let at = base
                        .checked_add(offset)
                        .ok_or(LlvmError::internal("an aggregate offset past 4GiB"))?;
                    let slot = self.byte_offset_ptr(buffer, at)?;
                    self.write_array_member(
                        *element,
                        *count,
                        stride,
                        field,
                        field_types[index],
                        slot,
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

    /// Rebuilds a Kira struct of type `ty` out of `buffer`'s C-layout bytes.
    /// Writes a struct's C-layout image into storage that outlives the call and
    /// leaves that storage's address.
    ///
    /// The buffer `write_aggregate_buffer` builds is an alloca — right for a
    /// by-value crossing, where C reads it during the call and the frame is
    /// gone afterwards. A pointer handed to C is the other case: the callee may
    /// keep it, so the image is copied somewhere that outlives the frame, and
    /// [`kira_runtime_abi::c_storage`] explains why that somewhere is never
    /// freed.
    pub(in crate::codegen) fn lower_clayout_address(
        &mut self,
        value: kira_ir::ir::IrExprId,
        id: ForeignAggregateId,
    ) -> Result<LLVMValueRef, LlvmError> {
        let ty = self.type_of(value);
        let lowered = self.lower_expr(value)?;
        let saved = self.call(self.codegen.runtime.stack_save, &mut [], c"clayout.stack");
        let buffer = self.write_aggregate_buffer(id, lowered, ty)?;
        let layout = self
            .codegen
            .program
            .foreign_aggregates
            .layout_of(id, self.codegen.pointer_width)
            .map_err(|_| LlvmError::internal("an aggregate with no computable C layout"))?;
        // SAFETY: `types.i64` belongs to this module's context.
        let size = unsafe { LLVMConstInt(self.codegen.types.i64, u64::from(layout.size), 0) };
        let block = self.call(
            self.codegen.runtime.cblock_bytes,
            &mut [buffer, size],
            c"clayout.block",
        );
        let llvm_type = self.codegen.llvm_type(ty)?;
        // SAFETY: the outer stack save below reclaims this source slot before
        // the expression returns, even when the call site is inside a loop.
        let source =
            unsafe { LLVMBuildAlloca(self.codegen.builder, llvm_type, c"clayout.source".as_ptr()) };
        // SAFETY: `source` holds exactly one value of `llvm_type`.
        unsafe { LLVMBuildStore(self.codegen.builder, lowered, source) };
        let zero = self.codegen.const_int(0);
        self.move_cblock_members(id, source, ty, block, zero)?;
        self.codegen.release_at(source, ty)?;
        self.call(self.codegen.runtime.stack_restore, &mut [saved], c"");
        Ok(block)
    }

    pub(super) fn read_aggregate_buffer(
        &mut self,
        id: ForeignAggregateId,
        buffer: LLVMValueRef,
        ty: Type,
    ) -> Result<LLVMValueRef, LlvmError> {
        self.read_members(id, buffer, 0, ty)
    }

    /// Reads one aggregate's members out of `buffer` at `base` into a value of
    /// Kira type `ty`.
    fn read_members(
        &mut self,
        id: ForeignAggregateId,
        buffer: LLVMValueRef,
        base: u32,
        ty: Type,
    ) -> Result<LLVMValueRef, LlvmError> {
        let members = self
            .codegen
            .program
            .foreign_aggregates
            .get(id)
            .ok_or(LlvmError::internal("an aggregate not in the table"))?
            .members()
            .to_vec();
        let field_types = self.struct_field_types(ty, members.len())?;

        let llvm_type = self.codegen.llvm_type(ty)?;
        // SAFETY: `llvm_type` is this struct's type in this live context.
        let mut value = unsafe { LLVMGetUndef(llvm_type) };
        let mut offset = 0u32;
        for (index, member) in members.iter().enumerate() {
            let field = match member {
                ForeignMember::Scalar(scalar) => {
                    let layout = scalar_layout(*scalar, self.codegen.pointer_width);
                    offset = round_up(offset, layout.align)?;
                    let at = base
                        .checked_add(offset)
                        .ok_or(LlvmError::internal("an aggregate offset past 4GiB"))?;
                    let slot = self.byte_offset_ptr(buffer, at)?;
                    let c_type = self.codegen.foreign_c_type(*scalar);
                    // SAFETY: `slot` addresses this scalar's bytes inside the
                    // buffer, which the adapter filled before returning.
                    let loaded = unsafe {
                        let load =
                            LLVMBuildLoad2(self.codegen.builder, c_type, slot, c"agg.f".as_ptr());
                        LLVMSetAlignment(load, layout.align);
                        load
                    };
                    offset = offset
                        .checked_add(layout.size)
                        .ok_or(LlvmError::internal("an aggregate larger than 4GiB"))?;
                    let field = self.codegen.c_value_to_kira(loaded, *scalar)?;
                    if self.c_storage_slot(ty, index)? {
                        self.call(
                            self.codegen.runtime.cblock_alien,
                            &mut [field],
                            c"aggregate.cblock.alien",
                        )
                    } else {
                        field
                    }
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
                    let at = base
                        .checked_add(offset)
                        .ok_or(LlvmError::internal("an aggregate offset past 4GiB"))?;
                    let inner = self.read_members(*nested, buffer, at, field_types[index])?;
                    offset = offset
                        .checked_add(layout.size)
                        .ok_or(LlvmError::internal("an aggregate larger than 4GiB"))?;
                    inner
                }
                ForeignMember::Array { element, count } => {
                    let (stride, align) = self.element_layout(*element)?;
                    offset = round_up(offset, align)?;
                    let at = base
                        .checked_add(offset)
                        .ok_or(LlvmError::internal("an aggregate offset past 4GiB"))?;
                    let slot = self.byte_offset_ptr(buffer, at)?;
                    let array =
                        self.read_array_member(*element, *count, stride, field_types[index], slot)?;
                    offset = offset
                        .checked_add(
                            stride
                                .checked_mul(*count)
                                .ok_or(LlvmError::internal("an aggregate larger than 4GiB"))?,
                        )
                        .ok_or(LlvmError::internal("an aggregate larger than 4GiB"))?;
                    array
                }
            };
            value = self.insert_field(value, field, index as u32)?;
        }
        Ok(value)
    }

    /// The Kira field types of the struct `ty`, checked against the member count
    /// the aggregate table holds for it.
    fn struct_field_types(&self, ty: Type, members: usize) -> Result<Vec<Type>, LlvmError> {
        let Type::Struct(struct_id) = ty else {
            return Err(LlvmError::internal(
                "an aggregate whose Kira type is not a struct",
            ));
        };
        let field_types: Vec<Type> = self
            .codegen
            .program
            .types
            .structs()
            .get(struct_id)
            .ok_or(LlvmError::internal("an aggregate naming no struct"))?
            .fields
            .iter()
            .map(|field| field.ty)
            .collect();
        if field_types.len() != members {
            return Err(LlvmError::internal(
                "an aggregate whose member count does not match its Kira struct",
            ));
        }
        Ok(field_types)
    }

    /// The stride and alignment of one inline-array element.
    fn element_layout(&self, element: ForeignArrayElement) -> Result<(u32, u32), LlvmError> {
        let layout = match element {
            ForeignArrayElement::Scalar(ty) => scalar_layout(ty, self.codegen.pointer_width),
            ForeignArrayElement::Aggregate(id) => self
                .codegen
                .program
                .foreign_aggregates
                .layout_of(id, self.codegen.pointer_width)
                .map_err(|_| LlvmError::internal("an aggregate with no computable C layout"))?,
        };
        Ok((layout.size, layout.align))
    }

    /// Writes a Kira array into `count` inline C elements starting at `base`.
    ///
    /// The buffer was zeroed, so a Kira array shorter than the C extent leaves
    /// the remaining elements zero — the same value a zero-filled construction
    /// carries, and what the VM's walk produces for the same array. A longer one
    /// traps: the elements past the extent have nowhere to go, and writing only
    /// the ones that fit would hand C a value the program did not write.
    fn write_array_member(
        &mut self,
        element: ForeignArrayElement,
        count: u32,
        stride: u32,
        array: LLVMValueRef,
        array_ty: Type,
        base: LLVMValueRef,
    ) -> Result<(), LlvmError> {
        let element_ty = self.codegen.element_of(array_ty)?;
        let esize = self.codegen.abi_size(element_ty)?;
        let len = self.call(self.codegen.runtime.array_len, &mut [array], c"agg.arr.len");
        self.trap_if_longer_than(len, count)?;
        let llvm_element = self.codegen.llvm_type(element_ty)?;
        self.emit_index_loop(len, |lowering, index| {
            let slot = lowering.call(
                lowering.codegen.runtime.array_slot,
                &mut [array, index, esize],
                c"agg.arr.slot",
            );
            // SAFETY: `slot` addresses a live element of `llvm_element`, and the
            // builder is on the loop body.
            let value = unsafe {
                LLVMBuildLoad2(
                    lowering.codegen.builder,
                    llvm_element,
                    slot,
                    c"agg.arr.elem".as_ptr(),
                )
            };
            let destination = lowering.element_ptr(base, index, stride)?;
            match element {
                ForeignArrayElement::Scalar(ty) => {
                    let converted = lowering.codegen.kira_value_to_c(value, ty)?;
                    let align = scalar_layout(ty, lowering.codegen.pointer_width).align;
                    // SAFETY: `destination` addresses this element's bytes inside
                    // the buffer, and `converted` has exactly this scalar's C
                    // type.
                    unsafe {
                        let store =
                            LLVMBuildStore(lowering.codegen.builder, converted, destination);
                        LLVMSetAlignment(store, align);
                    }
                    Ok(())
                }
                ForeignArrayElement::Aggregate(nested) => {
                    lowering.write_members(nested, value, element_ty, destination, 0)
                }
            }
        })
    }

    /// Reads `count` inline C elements starting at `base` into a fresh Kira
    /// array.
    ///
    /// Always the whole declared extent: C fixed storage carries no length of
    /// its own, so a shorter array would be a guess about which elements the
    /// callee meant.
    fn read_array_member(
        &mut self,
        element: ForeignArrayElement,
        count: u32,
        stride: u32,
        array_ty: Type,
        base: LLVMValueRef,
    ) -> Result<LLVMValueRef, LlvmError> {
        let element_ty = self.codegen.element_of(array_ty)?;
        let esize = self.codegen.abi_size(element_ty)?;
        let extent = self.codegen.const_int(i64::from(count));
        let handle = self.call(
            self.codegen.runtime.array_new,
            &mut [extent, esize],
            c"agg.arr.new",
        );
        self.emit_index_loop(extent, |lowering, index| {
            let source = lowering.element_ptr(base, index, stride)?;
            let value = match element {
                ForeignArrayElement::Scalar(ty) => {
                    let c_type = lowering.codegen.foreign_c_type(ty);
                    let align = scalar_layout(ty, lowering.codegen.pointer_width).align;
                    // SAFETY: `source` addresses this element's bytes inside the
                    // buffer the call filled, and `c_type` is the element's C
                    // type.
                    let loaded = unsafe {
                        let load = LLVMBuildLoad2(
                            lowering.codegen.builder,
                            c_type,
                            source,
                            c"agg.arr.c".as_ptr(),
                        );
                        LLVMSetAlignment(load, align);
                        load
                    };
                    lowering.codegen.c_value_to_kira(loaded, ty)?
                }
                ForeignArrayElement::Aggregate(nested) => {
                    lowering.read_members(nested, source, 0, element_ty)?
                }
            };
            let slot = lowering.call(
                lowering.codegen.runtime.array_slot,
                &mut [handle, index, esize],
                c"agg.arr.slot",
            );
            // SAFETY: `slot` is a fresh element slot of this array — allocated
            // full above, so the index is in range — and `value` has its type.
            unsafe { LLVMBuildStore(lowering.codegen.builder, value, slot) };
            Ok(())
        })?;
        Ok(handle)
    }

    /// A pointer to element `index` of an inline array starting at `base`.
    fn element_ptr(
        &mut self,
        base: LLVMValueRef,
        index: LLVMValueRef,
        stride: u32,
    ) -> Result<LLVMValueRef, LlvmError> {
        let types = self.codegen.types;
        let builder = self.codegen.builder;
        // SAFETY: the builder is on a live block; `index` is below the array's
        // extent, so `index * stride` stays inside the member's own bytes.
        Ok(unsafe {
            let stride = LLVMConstInt(types.i64, u64::from(stride), 0);
            let mut offset = LLVMBuildMul(builder, index, stride, c"agg.arr.off".as_ptr());
            LLVMBuildInBoundsGEP2(
                builder,
                types.i8,
                base,
                &raw mut offset,
                1,
                c"agg.arr.at".as_ptr(),
            )
        })
    }

    /// Traps when a Kira array holds more elements than the C extent takes.
    fn trap_if_longer_than(&mut self, len: LLVMValueRef, count: u32) -> Result<(), LlvmError> {
        let context = self.codegen.context;
        let builder = self.codegen.builder;
        let types = self.codegen.types;
        // SAFETY: the builder is on a live block of the function being lowered,
        // so it has a parent to append blocks to.
        let (overflow, ok, too_long) = unsafe {
            let function = LLVMGetBasicBlockParent(LLVMGetInsertBlock(builder));
            let overflow =
                LLVMAppendBasicBlockInContext(context, function, c"agg.arr.overflow".as_ptr());
            let ok = LLVMAppendBasicBlockInContext(context, function, c"agg.arr.fits".as_ptr());
            let extent = LLVMConstInt(types.i64, u64::from(count), 0);
            let too_long = LLVMBuildICmp(
                builder,
                llvm_sys::LLVMIntPredicate::LLVMIntSGT,
                len,
                extent,
                c"agg.arr.long".as_ptr(),
            );
            (overflow, ok, too_long)
        };
        // SAFETY: both blocks belong to this function and are still empty.
        unsafe {
            LLVMBuildCondBr(builder, too_long, overflow, ok);
            LLVMPositionBuilderAtEnd(builder, overflow);
        }
        let extent = self.codegen.const_int(i64::from(count));
        self.call(
            self.codegen.runtime.trap_foreign_array,
            &mut [extent, len],
            c"",
        );
        // SAFETY: the trap does not return, so the block ends here; the builder
        // then moves to the block the fitting case continues in.
        unsafe {
            LLVMBuildUnreachable(builder);
            LLVMPositionBuilderAtEnd(builder, ok);
        }
        Ok(())
    }

    /// Emits `for index in 0..limit { body }` around whatever `body` builds.
    ///
    /// The induction variable is a phi rather than an alloca, because this loop
    /// can nest — an array of structs holding their own arrays — and an alloca
    /// inside a loop grows the frame once per iteration.
    pub(super) fn emit_index_loop<F>(
        &mut self,
        limit: LLVMValueRef,
        body: F,
    ) -> Result<(), LlvmError>
    where
        F: FnOnce(&mut Self, LLVMValueRef) -> Result<(), LlvmError>,
    {
        let context = self.codegen.context;
        let builder = self.codegen.builder;
        let types = self.codegen.types;
        // SAFETY: the builder is on a live block of the function being lowered.
        let (head, body_block, done, index, entry) = unsafe {
            let function = LLVMGetBasicBlockParent(LLVMGetInsertBlock(builder));
            let head = LLVMAppendBasicBlockInContext(context, function, c"agg.arr.head".as_ptr());
            let body_block =
                LLVMAppendBasicBlockInContext(context, function, c"agg.arr.body".as_ptr());
            let done = LLVMAppendBasicBlockInContext(context, function, c"agg.arr.done".as_ptr());
            let entry = LLVMGetInsertBlock(builder);
            LLVMBuildBr(builder, head);
            LLVMPositionBuilderAtEnd(builder, head);
            let index = LLVMBuildPhi(builder, types.i64, c"agg.arr.i".as_ptr());
            let more = LLVMBuildICmp(
                builder,
                llvm_sys::LLVMIntPredicate::LLVMIntSLT,
                index,
                limit,
                c"agg.arr.more".as_ptr(),
            );
            LLVMBuildCondBr(builder, more, body_block, done);
            LLVMPositionBuilderAtEnd(builder, body_block);
            (head, body_block, done, index, entry)
        };

        body(self, index)?;

        // The body may have built blocks of its own, so the back edge leaves
        // whichever block the builder ended on, not `body_block`.
        // SAFETY: the builder is on an unterminated block reachable from the
        // loop body, and `head`'s phi is still open for its second incoming.
        unsafe {
            let latch = LLVMGetInsertBlock(builder);
            let next = LLVMBuildAdd(
                builder,
                index,
                LLVMConstInt(types.i64, 1, 0),
                c"agg.arr.next".as_ptr(),
            );
            LLVMBuildBr(builder, head);
            let mut incoming = [LLVMConstInt(types.i64, 0, 0), next];
            let mut blocks = [entry, latch];
            LLVMAddIncoming(index, incoming.as_mut_ptr(), blocks.as_mut_ptr(), 2);
            LLVMPositionBuilderAtEnd(builder, done);
        }
        let _ = body_block;
        Ok(())
    }

    /// A pointer `offset` bytes into `buffer`.
    fn byte_offset_ptr(
        &mut self,
        buffer: LLVMValueRef,
        offset: u32,
    ) -> Result<LLVMValueRef, LlvmError> {
        if offset == 0 {
            return Ok(buffer);
        }
        let types = self.codegen.types;
        let builder = self.codegen.builder;
        // SAFETY: `offset` is within the buffer by construction — it comes from
        // the same layout pass that sized the alloca — and `types.i8` makes the
        // index a byte index.
        Ok(unsafe {
            let mut index = LLVMConstInt(types.i64, u64::from(offset), 0);
            LLVMBuildInBoundsGEP2(
                builder,
                types.i8,
                buffer,
                &raw mut index,
                1,
                c"agg.at".as_ptr(),
            )
        })
    }
}

/// Rounds `value` up to the next multiple of `align`.
fn round_up(value: u32, align: u32) -> Result<u32, LlvmError> {
    value
        .checked_add(align - 1)
        .map(|raised| raised - (raised % align))
        .ok_or(LlvmError::internal("an aggregate larger than 4GiB"))
}
