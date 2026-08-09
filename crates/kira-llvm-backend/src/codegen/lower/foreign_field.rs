//! Reading one member through an `@FFI.Pointer`, in native code.
//!
//! The native half of `HirExpr::ForeignField`. A pointer that kept its target
//! makes this a plain load: the base is a pointer word, the member's byte offset
//! comes from the target's C layout, and what lands in a Kira local is the same
//! value the VM's `ForeignLoad` pushes.
//!
//! The offset is computed here rather than carried in the node because it is
//! target-dependent — a C pointer is four bytes on `wasm32` and eight elsewhere,
//! so a struct with a pointer member ahead of this one lays out differently per
//! target.
//!
//! The conversion to Kira's representation mirrors
//! [`super::super::foreign_scalar`]'s result handling, and has to: a member read
//! through a pointer and the same value returned from a C function are the same
//! seam scalar, so a difference between the two would be a difference between
//! two spellings of one fact.

use kira_runtime_abi::{ForeignAggregateId, ForeignMember, ForeignType};
use kira_semantics_model::Type;
use llvm_sys::core::*;
use llvm_sys::prelude::*;

use super::FunctionLowering;
use crate::LlvmError;

impl FunctionLowering<'_, '_> {
    /// Lowers the address of a member whose bytes live inside the container.
    pub(super) fn lower_foreign_member_address(
        &mut self,
        base: kira_ir::IrExprId,
        aggregate: ForeignAggregateId,
        member: u32,
    ) -> Result<LLVMValueRef, LlvmError> {
        let offset = self.foreign_member_offset(aggregate, member)?;
        let word = self.lower_expr(base)?;
        Ok(self.advance_foreign_pointer(word, offset))
    }

    /// Lowers `pointer[index]` to the address that many elements along.
    pub(super) fn lower_foreign_element(
        &mut self,
        base: kira_ir::IrExprId,
        aggregate: ForeignAggregateId,
        index: kira_ir::IrExprId,
    ) -> Result<LLVMValueRef, LlvmError> {
        let stride = self
            .codegen
            .program
            .foreign_aggregates
            .layout_of(aggregate, self.codegen.pointer_width)
            .map_err(|_| LlvmError::ForeignMemberMissing { member: 0 })?
            .size;
        let word = self.lower_expr(base)?;
        let index = self.lower_expr(index)?;
        let types = self.codegen.types;
        let builder = self.codegen.builder;
        // SAFETY: `word` is a pointer word and `index` is an `i64`, both on the
        // live current block.
        Ok(unsafe {
            let stride = LLVMConstInt(types.i64, u64::from(stride), 0);
            let step = LLVMBuildMul(builder, index, stride, c"elem.step".as_ptr());
            LLVMBuildAdd(builder, word, step, c"elem.at".as_ptr())
        })
    }

    /// The byte offset of one member of a C-layout aggregate.
    fn foreign_member_offset(
        &self,
        aggregate: ForeignAggregateId,
        member: u32,
    ) -> Result<u32, LlvmError> {
        self.codegen
            .program
            .foreign_aggregates
            .member_offsets_of(aggregate, self.codegen.pointer_width)
            .ok()
            .and_then(|offsets| offsets.get(member as usize).copied())
            .ok_or(LlvmError::ForeignMemberMissing { member })
    }

    /// Advances a pointer word by a constant byte offset.
    fn advance_foreign_pointer(&self, word: LLVMValueRef, offset: u32) -> LLVMValueRef {
        if offset == 0 {
            return word;
        }
        let types = self.codegen.types;
        // SAFETY: `word` is a pointer word on the live current block.
        unsafe {
            let by = LLVMConstInt(types.i64, u64::from(offset), 0);
            LLVMBuildAdd(self.codegen.builder, word, by, c"fld.at.word".as_ptr())
        }
    }

    /// Lowers `pointer.member` to a load from C memory.
    pub(super) fn lower_foreign_field(
        &mut self,
        base: kira_ir::IrExprId,
        aggregate: ForeignAggregateId,
        member: u32,
        ty: Type,
    ) -> Result<LLVMValueRef, LlvmError> {
        let offset = self.foreign_member_offset(aggregate, member)?;
        let seam = match self
            .codegen
            .program
            .foreign_aggregates
            .get(aggregate)
            .and_then(|entry| entry.members().get(member as usize).copied())
        {
            Some(ForeignMember::Scalar(seam)) => seam,
            _ => return Err(LlvmError::ForeignMemberMissing { member }),
        };

        let word = self.lower_expr(base)?;
        let types = self.codegen.types;
        let builder = self.codegen.builder;
        // SAFETY: `word` is a `RawPtr` value — an `i64` holding a pointer the
        // foreign seam produced — and `offset` plus the loaded type's size are
        // inside the target's own C layout.
        let value = unsafe {
            let pointer = LLVMBuildIntToPtr(builder, word, types.ptr, c"fld.ptr".as_ptr());
            let mut index = LLVMConstInt(types.i64, u64::from(offset), 0);
            let at = LLVMBuildInBoundsGEP2(
                builder,
                types.i8,
                pointer,
                &raw mut index,
                1,
                c"fld.at".as_ptr(),
            );
            let loaded = LLVMBuildLoad2(
                builder,
                self.codegen.foreign_c_type(seam),
                at,
                c"fld.load".as_ptr(),
            );
            self.widen_seam_scalar(loaded, seam, ty)
        };
        Ok(value)
    }

    /// Carries a loaded seam scalar into the representation Kira holds it in.
    ///
    /// # Safety
    ///
    /// `loaded` must have `seam`'s C type on the live current block.
    unsafe fn widen_seam_scalar(
        &self,
        loaded: LLVMValueRef,
        seam: ForeignType,
        ty: Type,
    ) -> LLVMValueRef {
        let types = self.codegen.types;
        let builder = self.codegen.builder;
        // SAFETY: the caller guarantees `loaded` has `seam`'s C type on the live
        // current block, which is what each conversion below is chosen for.
        unsafe {
            match seam {
                ForeignType::I8 | ForeignType::I16 | ForeignType::I32 => {
                    LLVMBuildSExt(builder, loaded, types.i64, c"fld.sext".as_ptr())
                }
                ForeignType::U8 | ForeignType::U16 | ForeignType::U32 => {
                    LLVMBuildZExt(builder, loaded, types.i64, c"fld.zext".as_ptr())
                }
                ForeignType::I64 | ForeignType::U64 => loaded,
                // A C `_Bool` is a byte; Kira's `Bool` is an `i1`, so the
                // comparison is the conversion.
                ForeignType::Bool => {
                    let zero = LLVMConstInt(LLVMTypeOf(loaded), 0, 0);
                    LLVMBuildICmp(
                        builder,
                        llvm_sys::LLVMIntPredicate::LLVMIntNE,
                        loaded,
                        zero,
                        c"fld.bool".as_ptr(),
                    )
                }
                ForeignType::F32 => LLVMBuildFPExt(builder, loaded, types.f64, c"fld.f64".as_ptr()),
                ForeignType::F64 => loaded,
                // A pointer member reads back as the pointer word Kira keeps a
                // `RawPtr` in, whether it was written `RawPtr` or as another
                // `@FFI.Pointer`.
                ForeignType::RawPtr | ForeignType::CString => {
                    LLVMBuildPtrToInt(builder, loaded, types.i64, c"fld.word".as_ptr())
                }
                // Refused where the read is analyzed: a `Void` member has no
                // bytes to load.
                ForeignType::Void => {
                    let _ = ty;
                    LLVMConstInt(types.i64, 0, 0)
                }
            }
        }
    }
}
