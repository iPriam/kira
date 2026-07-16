//! The host target machine, and emitting an object file with it.

use std::path::Path;

use llvm_sys::core::{LLVMDisposeMessage, LLVMSetTarget};
use llvm_sys::prelude::*;
use llvm_sys::target::{
    LLVM_InitializeNativeAsmPrinter, LLVM_InitializeNativeTarget, LLVMDisposeTargetData,
    LLVMSetModuleDataLayout,
};
use llvm_sys::target_machine::*;

use super::ffi::{c_string, take_message};
use crate::LlvmError;

/// The host target machine, used to emit objects.
pub(super) struct TargetMachine {
    machine: LLVMTargetMachineRef,
    triple: *mut std::os::raw::c_char,
}

impl TargetMachine {
    /// Builds a target machine for the compiling host.
    pub(super) fn host() -> Result<Self, LlvmError> {
        // SAFETY: the initializers are idempotent and safe to call repeatedly;
        // every out-parameter below is a live local, and each LLVM-owned string
        // is disposed of before returning or stored in `Self` for its drop.
        unsafe {
            if LLVM_InitializeNativeTarget() != 0 || LLVM_InitializeNativeAsmPrinter() != 0 {
                return Err(LlvmError::Emit(
                    "LLVM has no code generator for this host".to_owned(),
                ));
            }

            let triple = LLVMGetDefaultTargetTriple();
            let mut target: LLVMTargetRef = std::ptr::null_mut();
            let mut message: *mut std::os::raw::c_char = std::ptr::null_mut();
            if LLVMGetTargetFromTriple(triple, &mut target, &mut message) != 0 {
                let detail = take_message(message);
                LLVMDisposeMessage(triple);
                return Err(LlvmError::Emit(detail));
            }

            let cpu = LLVMGetHostCPUName();
            let features = LLVMGetHostCPUFeatures();
            let machine = LLVMCreateTargetMachine(
                target,
                triple,
                cpu,
                features,
                LLVMCodeGenOptLevel::LLVMCodeGenLevelDefault,
                // Host executables link position-independent everywhere Kira
                // targets; PIC is required on macOS and the default on modern
                // Linux distributions.
                LLVMRelocMode::LLVMRelocPIC,
                LLVMCodeModel::LLVMCodeModelDefault,
            );
            LLVMDisposeMessage(cpu);
            LLVMDisposeMessage(features);
            if machine.is_null() {
                LLVMDisposeMessage(triple);
                return Err(LlvmError::Emit(
                    "LLVM could not create a target machine for this host".to_owned(),
                ));
            }
            Ok(TargetMachine { machine, triple })
        }
    }

    /// Emits `module` as an object file at `path`.
    pub(super) fn emit_object(&self, module: LLVMModuleRef, path: &Path) -> Result<(), LlvmError> {
        let file = c_string(&path.to_string_lossy());
        // SAFETY: `module` and `self.machine` are live; the target/data-layout
        // are set from this same machine before emission, and LLVM allocates an
        // owned message only on failure.
        unsafe {
            LLVMSetTarget(module, self.triple);
            let layout = LLVMCreateTargetDataLayout(self.machine);
            LLVMSetModuleDataLayout(module, layout);
            LLVMDisposeTargetData(layout);

            let mut message: *mut std::os::raw::c_char = std::ptr::null_mut();
            let failed = LLVMTargetMachineEmitToFile(
                self.machine,
                module,
                file.as_ptr().cast_mut(),
                LLVMCodeGenFileType::LLVMObjectFile,
                &mut message,
            ) != 0;
            if failed {
                return Err(LlvmError::Emit(take_message(message)));
            }
        }
        Ok(())
    }
}

impl Drop for TargetMachine {
    fn drop(&mut self) {
        // SAFETY: both were created once in `host` and are released once here.
        unsafe {
            LLVMDisposeTargetMachine(self.machine);
            LLVMDisposeMessage(self.triple);
        }
    }
}
