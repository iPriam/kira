//! LLVM module ownership and module-wide lowering entry points.

use std::path::Path;

use kira_backend_api::NativeTarget;
use kira_debug::DebugInfo;
use kira_ir::IrProgram;
use kira_runtime_abi::{Execution, ForeignPointerWidth};
use llvm_sys::analysis::{LLVMVerifierFailureAction, LLVMVerifyModule};
use llvm_sys::core::*;

use super::ffi::{c_string, dispose_message, take_message};
use super::target::TargetMachine;
use super::{Codegen, CodegenTarget, CodegenUnit, Module, ModuleKind, Plan, needs_fast_codegen};
use crate::LlvmError;
use crate::exports::NativeExportSurface;

impl Module {
    /// Lowers a whole program into an LLVM module for a native executable.
    ///
    /// Every function is native here, whatever it was annotated: a whole-program
    /// native build has no VM half, so `@Runtime` marks a boundary this build
    /// does not have. That is the mirror of the VM-only build compiling every
    /// function to bytecode, and it is what keeps the two backends agreeing on
    /// any program.
    ///
    /// `unit` selects which function bodies land here; [`CodegenUnit::WHOLE`]
    /// is every one of them. `target` is the machine it is lowered and emitted
    /// for, which is [`NativeTarget::Host`] unless the build named another.
    pub(crate) fn build(
        program: &IrProgram,
        module_name: &str,
        pointer_width: ForeignPointerWidth,
        unavailable: &[usize],
        unit: CodegenUnit,
        target: &NativeTarget,
    ) -> Result<Self, LlvmError> {
        Self::build_executable_for_target(
            program,
            module_name,
            pointer_width,
            unavailable,
            unit,
            CodegenTarget::Native(target.clone()),
        )
    }

    /// Lowers a whole program into the shared library used by an LLVM live
    /// session.
    ///
    /// This has the same all-native function plan as [`Module::build`], but its
    /// entry is a fixed C symbol the desktop runner can load and call after the
    /// bundle crosses the process boundary.
    pub(crate) fn build_native_live(
        program: &IrProgram,
        module_name: &str,
        unavailable: &[usize],
        unit: CodegenUnit,
    ) -> Result<Self, LlvmError> {
        Self::lower(
            program,
            module_name,
            Plan {
                kind: ModuleKind::NativeLiveLibrary,
                engines: vec![Execution::Native; program.functions.len()],
                reachable: crate::reachability::native_functions(program),
                exports: &NativeExportSurface::default(),
                pointer_width: ForeignPointerWidth::HOST,
                target: CodegenTarget::host(),
                unavailable,
                unit,
            },
            None,
        )
    }

    /// Lowers a whole program with the Web target's data layout.
    pub(crate) fn build_wasm(
        program: &IrProgram,
        module_name: &str,
        pointer_width: ForeignPointerWidth,
        unavailable: &[usize],
        unit: CodegenUnit,
        device: kira_backend_api::WasmDevice,
    ) -> Result<Self, LlvmError> {
        Self::build_executable_for_target(
            program,
            module_name,
            pointer_width,
            unavailable,
            unit,
            CodegenTarget::Wasm(device),
        )
    }

    /// Lowers an executable for one target machine.
    fn build_executable_for_target(
        program: &IrProgram,
        module_name: &str,
        pointer_width: ForeignPointerWidth,
        unavailable: &[usize],
        unit: CodegenUnit,
        target: CodegenTarget,
    ) -> Result<Self, LlvmError> {
        Self::lower(
            program,
            module_name,
            Plan {
                kind: ModuleKind::Executable,
                engines: vec![Execution::Native; program.functions.len()],
                reachable: crate::reachability::native_functions(program),
                exports: &NativeExportSurface::default(),
                pointer_width,
                target,
                unavailable,
                unit,
            },
            None,
        )
    }

    /// Lowers a native executable while attaching debug identities and source
    /// locations from `debug`.
    pub(crate) fn build_debug(
        program: &IrProgram,
        module_name: &str,
        pointer_width: ForeignPointerWidth,
        unavailable: &[usize],
        unit: CodegenUnit,
        target: &NativeTarget,
        debug: &DebugInfo,
    ) -> Result<Self, LlvmError> {
        Self::lower(
            program,
            module_name,
            Plan {
                kind: ModuleKind::Executable,
                engines: vec![Execution::Native; program.functions.len()],
                reachable: crate::reachability::native_functions(program),
                exports: &NativeExportSurface::default(),
                pointer_width,
                target: CodegenTarget::Native(target.clone()),
                unavailable,
                unit,
            },
            Some(debug),
        )
    }

    /// Lowers a whole Kira library into an LLVM module with no entry point.
    ///
    /// Every function is native, exactly as in [`Module::build`]: the two
    /// differ only in whether a C `main` is emitted, which keeps a library and
    /// a program compiling their shared code through one path.
    pub(crate) fn build_library(
        program: &IrProgram,
        module_name: &str,
        exports: &NativeExportSurface,
        pointer_width: ForeignPointerWidth,
        target: &NativeTarget,
    ) -> Result<Self, LlvmError> {
        Self::build_library_for_target(
            program,
            module_name,
            exports,
            pointer_width,
            CodegenTarget::Native(target.clone()),
        )
    }

    /// Lowers a whole Kira library with the Web target's data layout.
    pub(crate) fn build_wasm_library(
        program: &IrProgram,
        module_name: &str,
        exports: &NativeExportSurface,
        device: kira_backend_api::WasmDevice,
    ) -> Result<Self, LlvmError> {
        let pointer_width = match device {
            kira_backend_api::WasmDevice::Wasm32 => ForeignPointerWidth::Bits32,
            kira_backend_api::WasmDevice::Wasm64 => ForeignPointerWidth::Bits64,
        };
        Self::build_library_for_target(
            program,
            module_name,
            exports,
            pointer_width,
            CodegenTarget::Wasm(device),
        )
    }

    /// Lowers a library for one target machine.
    fn build_library_for_target(
        program: &IrProgram,
        module_name: &str,
        exports: &NativeExportSurface,
        pointer_width: ForeignPointerWidth,
        target: CodegenTarget,
    ) -> Result<Self, LlvmError> {
        Self::lower(
            program,
            module_name,
            Plan {
                kind: ModuleKind::Library,
                engines: vec![Execution::Native; program.functions.len()],
                reachable: vec![true; program.functions.len()],
                exports,
                pointer_width,
                target,
                unavailable: &[],
                unit: CodegenUnit::WHOLE,
            },
            None,
        )
    }

    /// Lowers the native half of a hybrid program into a shared library.
    pub(crate) fn build_hybrid(
        program: &IrProgram,
        module_name: &str,
        unavailable: &[usize],
    ) -> Result<Self, LlvmError> {
        Self::build_hybrid_for_target(
            program,
            module_name,
            unavailable,
            ForeignPointerWidth::HOST,
            NativeTarget::Host,
        )
    }

    /// Lowers the native half of a hybrid program for one machine.
    ///
    /// The embedded-application build is the caller that needs this: its
    /// native half is linked *into* an app that runs on another machine, so
    /// the object must be that machine's — the same lowering, the target's
    /// data layout, and the target's code generator.
    pub(crate) fn build_hybrid_for_target(
        program: &IrProgram,
        module_name: &str,
        unavailable: &[usize],
        pointer_width: ForeignPointerWidth,
        target: NativeTarget,
    ) -> Result<Self, LlvmError> {
        Self::lower(
            program,
            module_name,
            Plan {
                kind: ModuleKind::HybridLibrary,
                engines: program
                    .functions
                    .iter()
                    .map(|function| function.execution.resolve(Execution::Runtime))
                    .collect(),
                reachable: crate::reachability::hybrid_native_functions(program),
                exports: &NativeExportSurface::default(),
                pointer_width,
                target: CodegenTarget::Native(target),
                unavailable,
                unit: CodegenUnit::WHOLE,
            },
            None,
        )
    }

    /// Lowers the native half of a hybrid library with native debug metadata.
    pub(crate) fn build_hybrid_debug(
        program: &IrProgram,
        module_name: &str,
        unavailable: &[usize],
        debug: &DebugInfo,
    ) -> Result<Self, LlvmError> {
        Self::lower(
            program,
            module_name,
            Plan {
                kind: ModuleKind::HybridLibrary,
                engines: program
                    .functions
                    .iter()
                    .map(|function| function.execution.resolve(Execution::Runtime))
                    .collect(),
                reachable: crate::reachability::hybrid_native_functions(program),
                exports: &NativeExportSurface::default(),
                pointer_width: ForeignPointerWidth::HOST,
                target: CodegenTarget::host(),
                unavailable,
                unit: CodegenUnit::WHOLE,
            },
            Some(debug),
        )
    }

    /// Builds the module.
    fn lower(
        program: &IrProgram,
        module_name: &str,
        plan: Plan<'_>,
        debug_info: Option<&DebugInfo>,
    ) -> Result<Self, LlvmError> {
        let fast_codegen = needs_fast_codegen(program, &plan);
        let target = plan.target.clone();
        let name = c_string(module_name);
        // SAFETY: the context, module, and builder are created together and
        // owned by the returned `Module`, which disposes of them on drop; each
        // call below receives objects from this same context.
        let owned = unsafe {
            let context = LLVMContextCreate();
            let module = LLVMModuleCreateWithNameInContext(name.as_ptr(), context);
            let builder = LLVMCreateBuilderInContext(context);
            Module {
                context,
                module,
                builder,
                fast_codegen,
                target,
            }
        };

        let mut codegen = Codegen::new(&owned, program, plan, debug_info)?;
        codegen.lower_program()?;
        owned.verify()?;
        Ok(owned)
    }

    /// Fails when LLVM considers the generated module malformed.
    fn verify(&self) -> Result<(), LlvmError> {
        let mut message: *mut std::os::raw::c_char = std::ptr::null_mut();
        // SAFETY: `self.module` is live; `LLVMVerifyModule` writes an owned
        // message we dispose of below, and reports failure through its return.
        let broken = unsafe {
            LLVMVerifyModule(
                self.module,
                LLVMVerifierFailureAction::LLVMReturnStatusAction,
                &mut message,
            ) != 0
        };
        if !broken {
            // SAFETY: the verifier may allocate a message even on success.
            unsafe { dispose_message(message) };
            return Ok(());
        }
        // SAFETY: on failure the verifier allocated a NUL-terminated message.
        let detail = unsafe { take_message(message) };
        if let Ok(path) = std::env::var("KIRA_DUMP_INVALID_LLVM_IR") {
            let _ = self.write_ir(Path::new(&path));
        }
        Err(LlvmError::InvalidModule(detail))
    }

    /// Writes the module's textual LLVM IR to `path`.
    pub(crate) fn write_ir(&self, path: &Path) -> Result<(), LlvmError> {
        let file = c_string(&path.to_string_lossy());
        let mut message: *mut std::os::raw::c_char = std::ptr::null_mut();
        // SAFETY: `self.module` is live and `file` is a NUL-terminated path;
        // LLVM writes an owned message only on failure.
        let failed =
            unsafe { LLVMPrintModuleToFile(self.module, file.as_ptr(), &mut message) != 0 };
        if failed {
            // SAFETY: LLVM allocated a NUL-terminated message on failure.
            let detail = unsafe { take_message(message) };
            return Err(LlvmError::Emit(detail));
        }
        Ok(())
    }

    /// Emits a native object file into `path`, for the machine this module was
    /// lowered against.
    pub(crate) fn emit_object(
        &self,
        path: &Path,
        optimize: bool,
        sanitize: crate::Sanitize,
    ) -> Result<(), LlvmError> {
        let unit = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("object");
        kira_diagnostics::progress!("creating LLVM target machine for {unit}");
        let machine = self.target_machine(optimize)?;
        if optimize {
            kira_diagnostics::progress!("inlining value glue for {unit}");
            machine.always_inline(self.module)?;
        }
        // After inlining, the way clang orders it: the checks land on the
        // loads and stores that survive, not on glue an inline erases.
        if sanitize == crate::Sanitize::Address {
            kira_diagnostics::progress!("instrumenting {unit} with AddressSanitizer");
            machine.address_sanitize(self.module)?;
        }
        kira_diagnostics::progress!(
            "emitting object with LLVM for {unit} (fast-codegen={})",
            self.fast_codegen
        );
        machine.emit_object(self.module, path)
    }

    /// Emits a WebAssembly object file for `device` into `path`.
    ///
    /// The same in-process emission as the host's — the target machine sets
    /// the module's wasm triple and data layout before emitting, so the object
    /// carries the Web device's pointer width, not this machine's.
    pub(crate) fn emit_wasm_object(
        &self,
        path: &Path,
        device: kira_backend_api::WasmDevice,
    ) -> Result<(), LlvmError> {
        let machine = TargetMachine::wasm(device)?;
        machine.emit_object(self.module, path)
    }

    /// The target machine for the machine this module was lowered against.
    ///
    /// The Web arm is here for completeness rather than for use: a wasm module
    /// is emitted through [`Module::emit_wasm_object`], which fixes the
    /// code-generation level the Web path has always run at instead of taking a
    /// build's `--release` and fast-codegen decisions.
    fn target_machine(&self, optimize: bool) -> Result<TargetMachine, LlvmError> {
        match &self.target {
            CodegenTarget::Native(target) => {
                TargetMachine::for_target(target, optimize, self.fast_codegen)
            }
            CodegenTarget::Wasm(device) => TargetMachine::wasm(*device),
        }
    }
}

impl Drop for Module {
    fn drop(&mut self) {
        // SAFETY: each object was created once in `build` and is disposed of
        // exactly once here; the module is disposed before its context, as LLVM
        // requires.
        unsafe {
            LLVMDisposeBuilder(self.builder);
            LLVMDisposeModule(self.module);
            LLVMContextDispose(self.context);
        }
    }
}
