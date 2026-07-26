//! Marshalling a Kira struct value into C-layout bytes and back, in native code.
//!
//! The native mirror of the VM's `value::aggregate` walk, and it has to produce
//! byte-identical buffers: the same program on `kirac run --backend vm` and
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

use kira_runtime_abi::{ForeignAggregateId, ForeignMember, scalar_layout};
use kira_semantics_model::Type;
use llvm_sys::core::*;
use llvm_sys::prelude::*;

use super::FunctionLowering;
use crate::LlvmError;

impl FunctionLowering<'_, '_> {
    /// Writes the Kira struct `value` into a fresh C-layout buffer.
    pub(super) fn write_aggregate_buffer(
        &mut self,
        id: ForeignAggregateId,
        value: LLVMValueRef,
    ) -> Result<LLVMValueRef, LlvmError> {
        let buffer = self.aggregate_alloca(id)?;
        let layout = self
            .codegen
            .program
            .foreign_aggregates
            .layout_of(id, self.codegen.pointer_width)
            .map_err(|_| LlvmError::Unsupported("an aggregate with no computable C layout"))?;
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
        self.write_members(id, value, buffer, 0)?;
        Ok(buffer)
    }

    /// Writes one aggregate's members into `buffer` at `base`.
    fn write_members(
        &mut self,
        id: ForeignAggregateId,
        value: LLVMValueRef,
        buffer: LLVMValueRef,
        base: u32,
    ) -> Result<(), LlvmError> {
        let members = self
            .codegen
            .program
            .foreign_aggregates
            .get(id)
            .ok_or(LlvmError::Unsupported("an aggregate not in the table"))?
            .members()
            .to_vec();
        let mut offset = 0u32;
        for (index, member) in members.iter().enumerate() {
            let field = self.extract_field(value, index as u32)?;
            match member {
                ForeignMember::Scalar(ty) => {
                    let layout = scalar_layout(*ty, self.codegen.pointer_width);
                    offset = round_up(offset, layout.align)?;
                    let at = base
                        .checked_add(offset)
                        .ok_or(LlvmError::Unsupported("an aggregate offset past 4GiB"))?;
                    let slot = self.byte_offset_ptr(buffer, at)?;
                    let converted = self.codegen.kira_value_to_c(field, *ty)?;
                    // SAFETY: `slot` addresses `layout.size` bytes inside the
                    // buffer, and `converted` has exactly this scalar's C type.
                    unsafe {
                        let store = LLVMBuildStore(self.codegen.builder, converted, slot);
                        LLVMSetAlignment(store, layout.align);
                    }
                    offset = offset
                        .checked_add(layout.size)
                        .ok_or(LlvmError::Unsupported("an aggregate larger than 4GiB"))?;
                }
                ForeignMember::Aggregate(nested) => {
                    let layout = self
                        .codegen
                        .program
                        .foreign_aggregates
                        .layout_of(*nested, self.codegen.pointer_width)
                        .map_err(|_| {
                            LlvmError::Unsupported("an aggregate with no computable C layout")
                        })?;
                    offset = round_up(offset, layout.align)?;
                    let at = base
                        .checked_add(offset)
                        .ok_or(LlvmError::Unsupported("an aggregate offset past 4GiB"))?;
                    self.write_members(*nested, field, buffer, at)?;
                    offset = offset
                        .checked_add(layout.size)
                        .ok_or(LlvmError::Unsupported("an aggregate larger than 4GiB"))?;
                }
            }
        }
        Ok(())
    }

    /// Rebuilds a Kira struct of type `ty` out of `buffer`'s C-layout bytes.
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
        let Type::Struct(struct_id) = ty else {
            return Err(LlvmError::Unsupported(
                "an aggregate result whose Kira type is not a struct",
            ));
        };
        let field_types: Vec<Type> = self
            .codegen
            .program
            .types
            .structs()
            .get(struct_id)
            .ok_or(LlvmError::Unsupported("an aggregate naming no struct"))?
            .fields
            .iter()
            .map(|field| field.ty)
            .collect();
        let members = self
            .codegen
            .program
            .foreign_aggregates
            .get(id)
            .ok_or(LlvmError::Unsupported("an aggregate not in the table"))?
            .members()
            .to_vec();
        if members.len() != field_types.len() {
            return Err(LlvmError::Unsupported(
                "an aggregate whose member count does not match its Kira struct",
            ));
        }

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
                        .ok_or(LlvmError::Unsupported("an aggregate offset past 4GiB"))?;
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
                        .ok_or(LlvmError::Unsupported("an aggregate larger than 4GiB"))?;
                    self.codegen.c_value_to_kira(loaded, *scalar)?
                }
                ForeignMember::Aggregate(nested) => {
                    let layout = self
                        .codegen
                        .program
                        .foreign_aggregates
                        .layout_of(*nested, self.codegen.pointer_width)
                        .map_err(|_| {
                            LlvmError::Unsupported("an aggregate with no computable C layout")
                        })?;
                    offset = round_up(offset, layout.align)?;
                    let at = base
                        .checked_add(offset)
                        .ok_or(LlvmError::Unsupported("an aggregate offset past 4GiB"))?;
                    let inner = self.read_members(*nested, buffer, at, field_types[index])?;
                    offset = offset
                        .checked_add(layout.size)
                        .ok_or(LlvmError::Unsupported("an aggregate larger than 4GiB"))?;
                    inner
                }
            };
            value = self.insert_field(value, field, index as u32)?;
        }
        Ok(value)
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
        .ok_or(LlvmError::Unsupported("an aggregate larger than 4GiB"))
}
