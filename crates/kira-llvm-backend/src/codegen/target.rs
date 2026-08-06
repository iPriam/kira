//! Target machines — the host's and the Web's — and emitting objects with them.

use std::path::Path;

use kira_backend_api::WasmDevice;
use llvm_sys::core::{LLVMCreateMessage, LLVMDisposeMessage, LLVMSetTarget};
use llvm_sys::prelude::*;
use llvm_sys::target::{LLVMDisposeTargetData, LLVMSetModuleDataLayout};
use llvm_sys::target_machine::*;

use super::ffi::{c_string, take_message};
use crate::LlvmError;

/// A target machine, used to emit objects.
pub(super) struct TargetMachine {
    machine: LLVMTargetMachineRef,
    triple: *mut std::os::raw::c_char,
}

/// Registers the code generators this backend emits with: the compiling
/// host's, and WebAssembly's when the managed LLVM carries it.
///
/// Explicit per-target calls rather than `LLVM_InitializeAll*`: those are C
/// wrapper functions `llvm-sys` only compiles when it owns the LLVM linking,
/// which this backend does itself (see `build.rs`). The explicit initializers
/// are real LLVM symbols living in their target's archive, so the set named
/// here is the set the linked bundle defines — `build.rs` reads it from that
/// bundle's own `llvm-config` and sets `kira_llvm_webassembly`. A bundle
/// published before the pin grew `WebAssembly` therefore links, and says so on
/// the Web path instead of failing four symbols into a link.
///
/// # Safety
///
/// The initializers are idempotent and safe to call repeatedly.
unsafe fn initialize_targets() {
    // SAFETY: per the function contract — idempotent registration calls.
    unsafe {
        #[cfg(target_arch = "aarch64")]
        {
            use llvm_sys::target::{
                LLVMInitializeAArch64AsmPrinter, LLVMInitializeAArch64Target,
                LLVMInitializeAArch64TargetInfo, LLVMInitializeAArch64TargetMC,
            };
            LLVMInitializeAArch64TargetInfo();
            LLVMInitializeAArch64Target();
            LLVMInitializeAArch64TargetMC();
            LLVMInitializeAArch64AsmPrinter();
        }
        #[cfg(target_arch = "x86_64")]
        {
            use llvm_sys::target::{
                LLVMInitializeX86AsmPrinter, LLVMInitializeX86Target, LLVMInitializeX86TargetInfo,
                LLVMInitializeX86TargetMC,
            };
            LLVMInitializeX86TargetInfo();
            LLVMInitializeX86Target();
            LLVMInitializeX86TargetMC();
            LLVMInitializeX86AsmPrinter();
        }
        #[cfg(kira_llvm_webassembly)]
        {
            use llvm_sys::target::{
                LLVMInitializeWebAssemblyAsmPrinter, LLVMInitializeWebAssemblyTarget,
                LLVMInitializeWebAssemblyTargetInfo, LLVMInitializeWebAssemblyTargetMC,
            };
            LLVMInitializeWebAssemblyTargetInfo();
            LLVMInitializeWebAssemblyTarget();
            LLVMInitializeWebAssemblyTargetMC();
            LLVMInitializeWebAssemblyAsmPrinter();
        }
    }
}

impl TargetMachine {
    /// Builds a target machine for the compiling host.
    ///
    /// `optimize` chooses the code-generation level: the default one, or the
    /// aggressive one when a build asks to be optimized.
    ///
    /// # There is no unoptimized level to choose
    ///
    /// Turning code generation off entirely emits far faster, and was the
    /// default here until it produced a compiler that could not run its own
    /// corpus. Without optimization LLVM gives every branch's locals a stack
    /// slot of its own rather than colouring them, so Project Matter's widget
    /// dispatch reserved a third of a megabyte on entry and thirty-four nested
    /// calls overflowed an 8 MB stack. The next level down from the default is
    /// not a middle ground either — measured on the editor it emits *slower*
    /// than the default and buys nothing.
    pub(super) fn host(optimize: bool) -> Result<Self, LlvmError> {
        // SAFETY: the initializers are idempotent and safe to call repeatedly;
        // every out-parameter below is a live local, and each LLVM-owned string
        // is disposed of before returning or stored in `Self` for its drop.
        unsafe {
            initialize_targets();

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
                if optimize {
                    LLVMCodeGenOptLevel::LLVMCodeGenLevelAggressive
                } else {
                    LLVMCodeGenOptLevel::LLVMCodeGenLevelDefault
                },
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
    /// `WebAssembly` reports [`LlvmError::WasmTargetMissing`] by name: once
    /// because the generator was never linked in, and once — for a bundle
    /// swapped underneath a compiler that did link it — because LLVM knows no
    /// such triple.
    pub(super) fn wasm(device: WasmDevice) -> Result<Self, LlvmError> {
        if !cfg!(kira_llvm_webassembly) {
            return Err(LlvmError::WasmTargetMissing);
        }
        let requested = match device {
            WasmDevice::Wasm32 => "wasm32-unknown-emscripten",
            WasmDevice::Wasm64 => "wasm64-unknown-emscripten",
        };
        // SAFETY: the target initializers are idempotent; the triple is an
        // owned message this struct's drop disposes, and the failure path
        // disposes everything it allocated.
        unsafe {
            initialize_targets();

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
