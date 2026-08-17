//! Target machines — the host's, a named cross target's, and the Web's — and
//! emitting objects with them.

use std::path::{Path, PathBuf};

use kira_backend_api::{CrossTarget, NativeTarget, RelocationModel, WasmDevice};
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

/// Registers every code generator the managed LLVM carries.
///
/// Explicit per-target calls rather than `LLVM_InitializeAll*`: those are C
/// wrapper functions `llvm-sys` only compiles when it owns the LLVM linking,
/// which this backend does itself (see `build.rs`). The explicit initializers
/// are real LLVM symbols living in their target's archive, so the set named
/// here is the set the linked bundle defines — `build.rs` reads it from that
/// bundle's own `llvm-config` and sets one cfg per generator. A bundle
/// published before the pin named a generator therefore links, and says so on
/// the path that needs it instead of failing four symbols into a link.
///
/// Nothing here is gated on `target_arch`. It was, and that made the set of
/// machines a compiler could emit for equal to the one machine it was built on,
/// which is the definition of a compiler that cannot cross-compile. What
/// decides now is what the *bundle* carries, so an x86_64 host linking a bundle
/// with AArch64 in it registers AArch64 and can emit for it.
///
/// Registration runs once per process. The initializers are idempotent when
/// called in sequence, but they write LLVM's global target registry, and a
/// program emitted in parallel codegen units builds a target machine on every
/// worker at once — two threads registering the same target concurrently is a
/// data race, not a repeated call.
///
/// # Safety
///
/// The initializers are idempotent and safe to call repeatedly.
unsafe fn initialize_targets() {
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    REGISTERED.call_once(|| {
        // SAFETY: `Once` runs this body exactly once per process, which is the
        // whole of what `register_targets` requires.
        unsafe {
            register_targets();
            llvm_sys::error_handling::LLVMInstallFatalErrorHandler(Some(report_llvm_fatal_error));
        }
    });
}

/// Says what happened when LLVM decides to end the process.
///
/// LLVM answers a fatal error — an unsupported construct reaching the code
/// generator, an allocation it cannot satisfy — by writing one line of its own
/// and calling `exit(1)`. Nothing after that runs: no error travels back up
/// through this crate, the build prints no diagnostic of its own, and what the
/// user sees is a compiler that stopped. This is the only place that can speak
/// before the exit, so it says which unit was being emitted and that the failure
/// is the compiler's, not the program's.
///
/// It cannot return an error instead. The call arrives from C++ with LLVM's own
/// frames below it, and unwinding through those is undefined; the exit is
/// LLVM's, and all this owns is the last word before it.
extern "C" fn report_llvm_fatal_error(reason: *const std::os::raw::c_char) {
    let detail = if reason.is_null() {
        "no reason given".to_owned()
    } else {
        // SAFETY: LLVM passes a live, NUL-terminated string that outlives this
        // call, and nothing here keeps a reference to it.
        unsafe { std::ffi::CStr::from_ptr(reason) }
            .to_string_lossy()
            .into_owned()
    };
    eprintln!(
        "kira: LLVM reported a fatal error while generating native code and ended the build:\n\
         {detail}\n\
         note: this is a defect in Kira's code generation, not in the program being built"
    );
}

/// The registration itself, run under [`initialize_targets`]'s `Once`.
///
/// # Safety
///
/// Must be called exactly once per process.
///
/// The asm *parser* is registered beside the asm printer, and it is not optional
/// decoration. Emitting an object containing inline assembly means assembling
/// that assembly, which LLVM does with the target's own parser; without it the
/// object emitter answers "Inline asm not supported by this streamer because we
/// don't have an asm parser for this target" and calls `exit`. That is the whole
/// of what a `@FFI.Syscall` lowers to, so a bundle whose generator is registered
/// but whose parser is not can emit for a machine right up until a program asks
/// the kernel for something.
unsafe fn register_targets() {
    // SAFETY: per the function contract — one registration per process.
    unsafe {
        #[cfg(kira_llvm_aarch64)]
        {
            use llvm_sys::target::{
                LLVMInitializeAArch64AsmParser, LLVMInitializeAArch64AsmPrinter,
                LLVMInitializeAArch64Target, LLVMInitializeAArch64TargetInfo,
                LLVMInitializeAArch64TargetMC,
            };
            LLVMInitializeAArch64TargetInfo();
            LLVMInitializeAArch64Target();
            LLVMInitializeAArch64TargetMC();
            LLVMInitializeAArch64AsmPrinter();
            LLVMInitializeAArch64AsmParser();
        }
        #[cfg(kira_llvm_x86)]
        {
            use llvm_sys::target::{
                LLVMInitializeX86AsmParser, LLVMInitializeX86AsmPrinter, LLVMInitializeX86Target,
                LLVMInitializeX86TargetInfo, LLVMInitializeX86TargetMC,
            };
            LLVMInitializeX86TargetInfo();
            LLVMInitializeX86Target();
            LLVMInitializeX86TargetMC();
            LLVMInitializeX86AsmPrinter();
            LLVMInitializeX86AsmParser();
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
    /// aggressive one when a build asks to be optimized. `fast_codegen` is the
    /// large-executable escape hatch: it uses LLVM's direct instruction
    /// selector, which avoids spending minutes colouring the enormous frames
    /// produced by a generated UI module.
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
    pub(super) fn host(optimize: bool, fast_codegen: bool) -> Result<Self, LlvmError> {
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
                codegen_level(optimize, fast_codegen),
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

    /// Builds a target machine for the machine `target` names, which is not
    /// this one.
    ///
    /// Three things differ from the host path, and each of them is the reason
    /// the two are separate functions rather than one with a flag:
    ///
    /// - the triple is the one the caller asked for, in the `arch-vendor-os-abi`
    ///   spelling LLVM takes, rather than `LLVMGetDefaultTargetTriple`;
    /// - the CPU is the architecture's generic one and the feature string is
    ///   empty, because `LLVMGetHostCPUName` answers about the processor running
    ///   the compiler and emitting *its* feature set for another machine
    ///   produces a binary that faults on an instruction the target has never
    ///   heard of;
    /// - the relocation model comes from the target instead of being fixed at
    ///   PIC, because a userland with no dynamic loader has nothing to apply the
    ///   relocations a position-independent image is full of.
    ///
    /// The generator is checked before LLVM is asked anything. A bundle built
    /// without the target's architecture reports
    /// [`LlvmError::TargetCodeGeneratorMissing`] by name, the same way the Web
    /// path reports its own missing generator — the alternative is
    /// `LLVMGetTargetFromTriple` answering "no available targets are compatible
    /// with triple", which names neither the bundle nor what to do about it.
    pub(super) fn cross(
        target: &CrossTarget,
        optimize: bool,
        fast_codegen: bool,
    ) -> Result<Self, LlvmError> {
        let requested = target.normalized_triple();
        check_supported(target)?;

        // SAFETY: the target initializers are idempotent; the triple is an
        // owned message this struct's drop disposes, and every failure path
        // disposes what it allocated before returning.
        unsafe {
            initialize_targets();

            let spelled = c_string(&requested);
            let triple = LLVMCreateMessage(spelled.as_ptr());
            let mut resolved: LLVMTargetRef = std::ptr::null_mut();
            let mut message: *mut std::os::raw::c_char = std::ptr::null_mut();
            if LLVMGetTargetFromTriple(triple, &mut resolved, &mut message) != 0 {
                LLVMDisposeMessage(message);
                LLVMDisposeMessage(triple);
                // The generator is linked and registered, so a refusal here is
                // about the triple itself rather than about the bundle: an
                // architecture Kira names and LLVM spells differently, or an
                // operating system this LLVM has no notion of.
                return Err(LlvmError::TargetTripleUnknown {
                    target: target.to_string(),
                    triple: requested,
                });
            }

            let cpu = c_string("generic");
            let features = c_string("");
            let machine = LLVMCreateTargetMachine(
                resolved,
                triple,
                cpu.as_ptr(),
                features.as_ptr(),
                codegen_level(optimize, fast_codegen),
                relocation_mode(target.relocation()),
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

    /// Builds the target machine a native build's selection asks for.
    ///
    /// The one place the host and cross paths are chosen between, so every
    /// caller that has a [`NativeTarget`] — the lowering that needs the module's
    /// data layout, and the emission that needs the code generator — makes the
    /// same choice from the same value.
    pub(super) fn for_target(
        target: &NativeTarget,
        optimize: bool,
        fast_codegen: bool,
    ) -> Result<Self, LlvmError> {
        match target {
            NativeTarget::Host => Self::host(optimize, fast_codegen),
            NativeTarget::Cross(cross) => Self::cross(cross, optimize, fast_codegen),
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

    /// Folds every `alwaysinline` function back into its callers.
    ///
    /// Copy and drop are emitted once per type and called (see
    /// [`super::glue`]), which is what keeps a development build's module small
    /// enough for LLVM to get through quickly. A release build wants the walk
    /// where it is used instead — a share count is a handful of instructions,
    /// and a call around it is most of its cost — so this puts it back. It is
    /// the only IR pass this backend runs, and it is run for exactly this.
    pub(super) fn always_inline(&self, module: LLVMModuleRef) -> Result<(), LlvmError> {
        use llvm_sys::transforms::pass_builder::*;

        let passes = c_string("always-inline");
        // SAFETY: `module` and `self.machine` are live; the options are created
        // and disposed here, and LLVM allocates an error only on failure.
        unsafe {
            let options = LLVMCreatePassBuilderOptions();
            let error = LLVMRunPasses(module, passes.as_ptr(), self.machine, options);
            LLVMDisposePassBuilderOptions(options);
            if !error.is_null() {
                let detail = take_message(llvm_sys::error::LLVMGetErrorMessage(error));
                return Err(LlvmError::Emit(detail));
            }
        }
        Ok(())
    }

    /// Emits `module` as an object file at `path`.
    ///
    /// Written beside the object and renamed onto it, so `path` only ever holds
    /// a *finished* object. LLVM creates the file before it fills it, and an
    /// emission that ends early — a fatal error, a process killed mid-build —
    /// otherwise leaves a truncated one behind, which the next link reads as an
    /// object with no symbols in it and reports as the program's fault.
    pub(super) fn emit_object(&self, module: LLVMModuleRef, path: &Path) -> Result<(), LlvmError> {
        let pending = pending_path(path);
        let file = c_string(&pending.to_string_lossy());
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
                let detail = take_message(message);
                let _ = std::fs::remove_file(&pending);
                return Err(LlvmError::Emit(detail));
            }
        }
        std::fs::rename(&pending, path).map_err(|error| {
            LlvmError::Emit(format!(
                "cannot move the emitted object into `{}`: {error}",
                path.display()
            ))
        })
    }
}

/// Reports whether this compiler carries a code generator for `target`, without
/// building anything or touching LLVM.
///
/// Asked before a build starts as well as inside [`TargetMachine::cross`]. A
/// cross build has several things that must be in place — the code generator,
/// the runtime archive for that machine, the sysroot — and this is the one that
/// no amount of arranging the machine can fix: the others are files to fetch or
/// paths to set, while a missing generator means a different compiler binary.
/// Reporting it first is what keeps a user from installing an aarch64 runtime
/// archive to satisfy a build that was never going to emit aarch64 code.
pub(crate) fn check_supported(target: &CrossTarget) -> Result<(), LlvmError> {
    let Some(generator) =
        kira_toolchain::llvm_code_generators::code_generator_for_arch(target.triple().arch())
    else {
        return Err(LlvmError::TargetArchitectureUnknown {
            target: target.to_string(),
            arch: target.triple().arch().to_owned(),
        });
    };
    if !code_generator_linked(generator) {
        return Err(LlvmError::TargetCodeGeneratorMissing {
            target: target.to_string(),
            generator,
        });
    }
    Ok(())
}

/// Whether the managed LLVM this compiler links defines `generator`'s
/// initializers.
///
/// The cfgs are the build script's reading of the bundle's own
/// `llvm-config --targets-built`, so this answers about the archives that are
/// actually linked in rather than about what the pin asks for. Anything Kira
/// has no registration for at all answers `false`, which is the honest answer:
/// an unregistered generator is one LLVM will not resolve a triple against even
/// if the code is present.
fn code_generator_linked(generator: &str) -> bool {
    matches!(generator, "X86" if cfg!(kira_llvm_x86))
        || matches!(generator, "AArch64" if cfg!(kira_llvm_aarch64))
}

/// The code-generation level a build asks for.
///
/// Shared by the host and cross paths so the `--release` and fast-codegen
/// decisions cannot come out differently depending on which machine is being
/// emitted for. See [`TargetMachine::host`] for why there is no unoptimized
/// level to choose.
fn codegen_level(optimize: bool, fast_codegen: bool) -> LLVMCodeGenOptLevel {
    if fast_codegen && !optimize {
        LLVMCodeGenOptLevel::LLVMCodeGenLevelNone
    } else if optimize {
        LLVMCodeGenOptLevel::LLVMCodeGenLevelAggressive
    } else {
        LLVMCodeGenOptLevel::LLVMCodeGenLevelDefault
    }
}

/// LLVM's spelling of a target's relocation model.
fn relocation_mode(relocation: RelocationModel) -> LLVMRelocMode {
    match relocation {
        RelocationModel::Pic => LLVMRelocMode::LLVMRelocPIC,
        RelocationModel::Static => LLVMRelocMode::LLVMRelocStatic,
    }
}

/// Where an object is written before it is complete.
///
/// Beside the object it becomes, so the rename that finishes it stays within one
/// directory and one filesystem, and named after the writing process so two
/// builds — of two packages, or a build and a `kira test` — never share the
/// partial file even though the object they produce has the same name.
fn pending_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".pending-{}", std::process::id()));
    path.with_file_name(name)
}

impl Drop for TargetMachine {
    fn drop(&mut self) {
        // SAFETY: both were created once by one of the constructors above and
        // are released once here.
        unsafe {
            LLVMDisposeTargetMachine(self.machine);
            LLVMDisposeMessage(self.triple);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kira_native_lib_definition::TargetTriple;

    fn cross_target(text: &str, relocation: RelocationModel) -> CrossTarget {
        CrossTarget::new(
            TargetTriple::parse(text).expect("a valid triple"),
            relocation,
            kira_backend_api::Linkage::Dynamic,
        )
    }

    /// An architecture Kira has no code generator name for is refused by name,
    /// before LLVM is asked anything, so the diagnostic says which target and
    /// which architecture rather than reporting an unresolvable triple.
    #[test]
    fn a_target_kira_has_no_code_generator_for_is_named() {
        // Matched rather than unwrapped: a `TargetMachine` owns LLVM handles
        // and has no `Debug`, so there is nothing for `expect_err` to print.
        let Err(error) = TargetMachine::cross(
            &cross_target("mips64-linux-gnu", RelocationModel::Pic),
            false,
            false,
        ) else {
            panic!("Kira publishes no mips64 code generator");
        };
        let text = error.to_string();
        assert!(text.contains("mips64-linux-gnu"), "{text}");
        assert!(text.contains("mips64"), "{text}");
    }

    /// The host's own generator is always linked: `build.rs` refuses a bundle
    /// without it outright, since such a bundle can emit for nothing at all.
    #[test]
    fn this_hosts_code_generator_is_always_linked() {
        let host = kira_toolchain::llvm_code_generators::host_code_generator()
            .expect("Kira publishes a bundle for this host");
        assert!(
            code_generator_linked(host),
            "the {host} code generator must be linked into a compiler built for this host",
        );
    }

    /// The relocation model reaches LLVM as the model that was asked for. A
    /// silent PIC would produce a program that starts on a machine with a
    /// dynamic loader and faults on one without.
    #[test]
    fn each_relocation_model_maps_to_its_own_llvm_mode() {
        assert_eq!(
            relocation_mode(RelocationModel::Pic),
            LLVMRelocMode::LLVMRelocPIC
        );
        assert_eq!(
            relocation_mode(RelocationModel::Static),
            LLVMRelocMode::LLVMRelocStatic
        );
    }

    /// Optimization decides the level; fast codegen only lowers it for a build
    /// that did not ask to be optimized.
    #[test]
    fn the_codegen_level_follows_optimization_before_fast_codegen() {
        assert_eq!(
            codegen_level(true, true),
            LLVMCodeGenOptLevel::LLVMCodeGenLevelAggressive
        );
        assert_eq!(
            codegen_level(false, true),
            LLVMCodeGenOptLevel::LLVMCodeGenLevelNone
        );
        assert_eq!(
            codegen_level(false, false),
            LLVMCodeGenOptLevel::LLVMCodeGenLevelDefault
        );
    }
}
