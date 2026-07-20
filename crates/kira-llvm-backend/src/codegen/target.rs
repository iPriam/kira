//! Target machines — the host's and the Web's — and emitting objects with them.

use std::path::Path;

use kira_backend_api::WasmDevice;
use llvm_sys::core::{LLVMCreateMessage, LLVMDisposeMessage, LLVMSetTarget};
use llvm_sys::prelude::*;
use llvm_sys::target::{
    LLVM_InitializeAllAsmPrinters, LLVM_InitializeAllTargetInfos, LLVM_InitializeAllTargetMCs,
    LLVM_InitializeAllTargets, LLVM_InitializeNativeAsmPrinter, LLVM_InitializeNativeTarget,
    LLVMDisposeTargetData, LLVMSetModuleDataLayout,
};
use llvm_sys::target_machine::*;

use super::ffi::{c_string, take_message};
use crate::LlvmError;

/// A target machine, used to emit objects.
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

    /// Builds a target machine for a Web device.
    ///
    /// Emission is in-process through the same C API as the host's — never a
    /// textual-IR round trip — so it needs the WebAssembly code generator
    /// compiled into the managed LLVM. A bundle built before the pin included
    /// `WebAssembly` reports [`LlvmError::WasmTargetMissing`] by name.
    pub(super) fn wasm(device: WasmDevice) -> Result<Self, LlvmError> {
        let requested = match device {
            WasmDevice::Wasm32 => "wasm32-unknown-emscripten",
            WasmDevice::Wasm64 => "wasm64-unknown-emscripten",
        };
        // SAFETY: the all-target initializers register whatever code
        // generators this LLVM was built with and are idempotent; the triple
        // is an owned message this struct's drop disposes, and the failure
        // path disposes everything it allocated.
        unsafe {
            LLVM_InitializeAllTargetInfos();
            LLVM_InitializeAllTargets();
            LLVM_InitializeAllTargetMCs();
            LLVM_InitializeAllAsmPrinters();

            let spelled = c_string(requested);
            let triple = LLVMCreateMessage(spelled.as_ptr());
            let mut target: LLVMTargetRef = std::ptr::null_mut();
            let mut message: *mut std::os::raw::c_char = std::ptr::null_mut();
            if LLVMGetTargetFromTriple(triple, &mut target, &mut message) != 0 {
                LLVMDisposeMessage(message);
                LLVMDisposeMessage(triple);
                return Err(LlvmError::WasmTargetMissing);
            }

            let cpu = c_string("generic");
            let features = c_string("");
            let machine = LLVMCreateTargetMachine(
                target,
                triple,
                cpu.as_ptr(),
                features.as_ptr(),
                LLVMCodeGenOptLevel::LLVMCodeGenLevelDefault,
                LLVMRelocMode::LLVMRelocDefault,
                LLVMCodeModel::LLVMCodeModelDefault,
            );
            if machine.is_null() {
                LLVMDisposeMessage(triple);
                return Err(LlvmError::Emit(format!(
                    "LLVM could not create a target machine for `{requested}`"
                )));
            }
            Ok(TargetMachine { machine, triple })
        }
    }

    /// Sets `module`'s target triple and data layout to this host's.
    ///
    /// Setting copies the layout into the module, so the temporary handle is
    /// disposed immediately; a borrowed reference to the module's own copy
    /// (via `LLVMGetModuleDataLayout`) stays valid for the module's lifetime.
    pub(super) fn set_module_layout(&self, module: LLVMModuleRef) {
        // SAFETY: `module` and `self.machine` are live; the layout is set from
        // this same machine and the temporary handle is disposed right after.
        unsafe {
            LLVMSetTarget(module, self.triple);
            let layout = LLVMCreateTargetDataLayout(self.machine);
            LLVMSetModuleDataLayout(module, layout);
            LLVMDisposeTargetData(layout);
        }
    }

    /// Emits `module` as an object file at `path`.
    pub(super) fn emit_object(&self, module: LLVMModuleRef, path: &Path) -> Result<(), LlvmError> {
        let file = c_string(&path.to_string_lossy());
        self.set_module_layout(module);
        // SAFETY: `module` and `self.machine` are live, its data layout was set
        // just above, and LLVM allocates an owned message only on failure.
        unsafe {
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
