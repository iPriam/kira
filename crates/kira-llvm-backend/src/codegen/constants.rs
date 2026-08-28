//! Module-scope constants in the native backend.
//!
//! Each constant owns one LLVM global, shaped by its Kira type and filled once
//! by `kira.constants.init`, which calls the constants' init functions front to
//! back in the program table's order — the compiler's dependency order. An
//! executable's entry calls the init before `@Main` and releases the slots
//! after it returns; a consumer-entered library registers the init as a global
//! constructor instead, because its consumer owns the entry and cannot be asked
//! to start the module.
//!
//! The hybrid native half is the one place with no globals at all: its VM half
//! owns the constants, so a read there crosses the bridge to the constant's
//! init function — the VM computes the slot's value in its own table order and
//! hands the result back. That path is in [`super::lower`].

use llvm_sys::LLVMLinkage;
use llvm_sys::core::{
    LLVMAddFunction, LLVMAddGlobal, LLVMAppendBasicBlockInContext, LLVMArrayType2, LLVMBuildCall2,
    LLVMBuildRetVoid, LLVMBuildStore, LLVMConstArray2, LLVMConstInt, LLVMConstNamedStruct,
    LLVMConstNull, LLVMConstPointerNull, LLVMFunctionType, LLVMGetInsertBlock,
    LLVMPositionBuilderAtEnd, LLVMSetInitializer, LLVMSetLinkage, LLVMStructTypeInContext,
};
use llvm_sys::prelude::{LLVMTypeRef, LLVMValueRef};

use super::ffi::c_string;
use super::{Codegen, ModuleKind};
use crate::LlvmError;

impl Codegen<'_> {
    /// Declares one global per module constant, shaped by the constant's type.
    ///
    /// Every codegen unit declares them so a read in any unit can load; only
    /// the first unit defines them (zero-filled until the init runs). The
    /// hybrid native half declares none — its reads bridge to the VM instead.
    pub(super) fn declare_constant_globals(&mut self) -> Result<(), LlvmError> {
        if self.kind == ModuleKind::HybridLibrary {
            return Ok(());
        }
        for (index, constant) in self.program.constants.iter().enumerate() {
            let ty = self.llvm_type(constant.ty)?;
            let name = c_string(&format!("kira.constant.{index}.{}", constant.name));
            // SAFETY: the module is live and `name` outlives the call, which
            // copies it.
            let global = unsafe { LLVMAddGlobal(self.module, ty, name.as_ptr()) };
            if self.unit.is_first() {
                // SAFETY: `global` was just added to this module and `ty` is
                // its own type; a null constant of it is the zero fill.
                unsafe { LLVMSetInitializer(global, LLVMConstNull(ty)) };
            }
            self.constant_globals.push(global);
        }
        Ok(())
    }

    /// The global backing constant slot `index`, when this module carries one.
    pub(super) fn constant_global(&self, index: u32) -> Option<LLVMValueRef> {
        self.constant_globals.get(index as usize).copied()
    }

    /// Emits `kira.constants.init`: one call per constant, front to back, each
    /// result stored into its global. Returns `None` for a program with no
    /// constants, so callers emit nothing at all for the common case.
    pub(super) fn lower_constants_init(&mut self) -> Result<Option<LLVMValueRef>, LlvmError> {
        if self.program.constants.is_empty() || self.kind == ModuleKind::HybridLibrary {
            return Ok(None);
        }
        // The init is emitted between whatever else is being built, so the
        // builder's position is saved and restored around it.
        // SAFETY: the builder is live; null means it was not positioned yet.
        let resume = unsafe { LLVMGetInsertBlock(self.builder) };
        // SAFETY: the module and context are live; the function and its entry
        // block belong to them.
        let init = unsafe {
            let init_ty = LLVMFunctionType(self.types.void, std::ptr::null_mut(), 0, 0);
            let init = LLVMAddFunction(self.module, c"kira.constants.init".as_ptr(), init_ty);
            let block = LLVMAppendBasicBlockInContext(self.context, init, c"entry".as_ptr());
            LLVMPositionBuilderAtEnd(self.builder, block);
            init
        };
        for (index, constant) in self.program.constants.iter().enumerate() {
            let target = self.functions[constant.init as usize].ok_or(LlvmError::internal(
                "a constant init with no native body outside the hybrid half",
            ))?;
            let global = self.constant_globals[index];
            let name = c_string(&format!("kira.constant.{index}.value"));
            // SAFETY: the callee was declared in this module with a
            // zero-argument type, and the builder is on the init's entry block.
            unsafe {
                let value = LLVMBuildCall2(
                    self.builder,
                    target.ty,
                    target.value,
                    std::ptr::null_mut(),
                    0,
                    name.as_ptr(),
                );
                LLVMBuildStore(self.builder, value, global);
            }
        }
        // SAFETY: the builder is on the init's entry block; `resume` is the
        // block the caller was building into, or null when there was none.
        unsafe {
            LLVMBuildRetVoid(self.builder);
            if !resume.is_null() {
                LLVMPositionBuilderAtEnd(self.builder, resume);
            }
        }
        Ok(Some(init))
    }

    /// Emits the release of every constant global, newest slot first, at the
    /// builder's current position.
    ///
    /// The mirror of the init, run by an entry after `@Main` returns so a
    /// clean program's heap accounting still balances. A consumer-entered
    /// library skips this: its constants live as long as the process does.
    pub(super) fn lower_constants_release(&mut self) -> Result<(), LlvmError> {
        for index in (0..self.program.constants.len()).rev() {
            let ty = self.program.constants[index].ty;
            let global = self.constant_globals[index];
            self.release_at(global, ty)?;
        }
        Ok(())
    }

    /// Registers `init` in `llvm.global_ctors`, so a consumer-entered library
    /// fills its constants when it is loaded.
    pub(super) fn register_constants_ctor(&mut self, init: LLVMValueRef) {
        // SAFETY: every type and constant below belongs to this live module's
        // context, and the ctor entry references a function of this module.
        unsafe {
            let ptr = self.types.ptr;
            let mut fields: [LLVMTypeRef; 3] = [self.types.i32, ptr, ptr];
            let entry_ty =
                LLVMStructTypeInContext(self.context, fields.as_mut_ptr(), fields.len() as u32, 0);
            let mut values: [LLVMValueRef; 3] = [
                // Default priority: nothing else in the module uses ctors, so
                // there is no order to negotiate.
                LLVMConstInt(self.types.i32, 65535, 0),
                init,
                LLVMConstPointerNull(ptr),
            ];
            let entry = LLVMConstNamedStruct(entry_ty, values.as_mut_ptr(), values.len() as u32);
            let array_ty = LLVMArrayType2(entry_ty, 1);
            let mut entries = [entry];
            let array = LLVMConstArray2(entry_ty, entries.as_mut_ptr(), 1);
            let global = LLVMAddGlobal(self.module, array_ty, c"llvm.global_ctors".as_ptr());
            LLVMSetInitializer(global, array);
            LLVMSetLinkage(global, LLVMLinkage::LLVMAppendingLinkage);
        }
    }
}
