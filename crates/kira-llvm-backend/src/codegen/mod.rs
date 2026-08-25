//! Lowering a verified [`IrProgram`] into an LLVM module, and emitting it as a
//! native object.
//!
//! This module owns the LLVM objects (context, module, builder) and the
//! program-wide scaffold: one declaration per Kira function, and the plan for
//! which engine owns which body. The pieces around it each own one job:
//!
//! - [`types`] — the LLVM types Kira's values map onto, and the `kira_rt_*`
//!   declarations,
//! - [`lower`] — statement and expression lowering, which must agree with the
//!   interpreter instruction for instruction,
//! - [`entry`] — how the module is entered: a C `main`, or host trampolines,
//! - [`bridge`] — packing and unpacking values that cross between engines,
//! - [`symbols`] — the native symbol each function is emitted under,
//! - [`target`] — the target machine, host or cross, and object emission,
//! - [`ffi`] — glue for the LLVM C API's strings.
//!
//! Every LLVM object here is a raw pointer from the C API, so the whole module
//! is one `unsafe` fence: [`Module`] owns its context and disposes of it on
//! drop, and no LLVM reference escapes that lifetime.

mod boxing;
mod bridge;
mod callback;
mod debug;
mod elements;
mod entry;
mod ffi;
mod foreign_scalar;
mod glue;
mod library;
mod lower;
mod native_ffi;
mod native_state;
mod native_state_enums;
mod native_state_values;
mod plan;
mod symbols;
mod syscall;
mod target;
mod types;
mod values;
mod widening;

use std::collections::{BTreeSet, HashMap};
use std::ffi::CStr;
use std::path::Path;

use kira_backend_api::NativeTarget;
use kira_debug::DebugInfo;
use kira_ir::{IrFunction, IrProgram};
use kira_runtime_abi::{Execution, ForeignPointerWidth};
use kira_semantics_model::Type;
use llvm_sys::LLVMTypeKind;
use llvm_sys::analysis::{LLVMVerifierFailureAction, LLVMVerifyModule};
use llvm_sys::core::*;
use llvm_sys::prelude::*;
use llvm_sys::target::{LLVMGetModuleDataLayout, LLVMTargetDataRef};

use self::elements::Leaf;
use self::native_state::StateLeaf;

use self::ffi::{c_string, dispose_message, take_message};
use self::symbols::symbol_name;
use self::target::TargetMachine;
use self::types::{Runtime, Types, declare_runtime};
use crate::LlvmError;
use crate::exports::NativeExportSurface;

use self::plan::{CodegenTarget, ModuleKind, Plan};

pub(crate) use self::plan::CodegenUnit;
pub(crate) use self::symbols::trampoline_name;
pub(crate) use self::target::check_supported;
pub(crate) use self::types::Callable;

/// The direct LLVM selector is substantially faster for a large executable
/// module. Smaller programs keep the normal codegen level, which is important
/// because it colours stack slots and is what keeps ordinary native programs'
/// frames compact.
const FAST_CODEGEN_REACHABLE_FUNCTIONS: usize = 1_000;

fn needs_fast_codegen(program: &IrProgram, plan: &Plan<'_>) -> bool {
    // Never for a cross build. This is a build-time escape hatch, bought by
    // emitting worse code, and it is worth that on the machine a developer is
    // waiting at. A binary being produced for another machine is one nobody is
    // waiting to run here, and the frames the direct selector leaves behind are
    // the ones that overflowed an 8 MB stack.
    if !matches!(plan.target, CodegenTarget::Native(NativeTarget::Host)) {
        return false;
    }
    if !cfg!(target_os = "windows") || plan.kind != ModuleKind::Executable {
        return false;
    }
    let native_reachable = program
        .functions
        .iter()
        .enumerate()
        .filter(|(index, _)| {
            plan.engines.get(*index) == Some(&Execution::Native)
                && plan.reachable.get(*index).copied().unwrap_or(false)
        })
        .count();
    native_reachable >= FAST_CODEGEN_REACHABLE_FUNCTIONS
}

/// An LLVM module holding a lowered Kira program.
///
/// Owns its LLVM context; dropping it disposes of every LLVM object built from
/// it, which is why no reference into the module outlives this value.
pub(crate) struct Module {
    context: LLVMContextRef,
    module: LLVMModuleRef,
    builder: LLVMBuilderRef,
    fast_codegen: bool,
    /// The machine this module was lowered against, kept so the emission uses
    /// the same one.
    ///
    /// Carried rather than passed in again at emission time: the data layout the
    /// offsets were computed with and the target machine the object comes out of
    /// have to be the same target, and a second argument is a second chance for
    /// them not to be.
    target: CodegenTarget,
}

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
    pub(crate) fn emit_object(&self, path: &Path, optimize: bool) -> Result<(), LlvmError> {
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

/// Lowers a program into an owned [`Module`].
pub(crate) struct Codegen<'a> {
    program: &'a IrProgram,
    /// Imports whose library is absent on this target; their adapters return a
    /// status instead of calling a symbol that would not link.
    unavailable: Vec<usize>,
    /// The pointer width of the target this module is emitted for.
    ///
    /// Baked into every C-layout aggregate offset the lowering computes, so it
    /// has to be the *target*'s width, not this machine's: a wasm32 module
    /// built on a 64-bit host lays a pointer member out in four bytes.
    pointer_width: ForeignPointerWidth,
    /// What this module is emitted for.
    ///
    /// A foreign call reaches its C differently on each: a host build calls
    /// through the bundled libffi graph, and a wasm module calls the symbol
    /// directly, because its C is linked into the module and there is no
    /// loader, no second image, and no libffi to reach.
    target: CodegenTarget,
    context: LLVMContextRef,
    module: LLVMModuleRef,
    builder: LLVMBuilderRef,
    types: Types,
    runtime: Runtime,
    /// What this module is being built as.
    kind: ModuleKind,
    /// Which of the program's function bodies this module carries.
    unit: CodegenUnit,
    /// Which program functions can be reached by this module.
    reachable: Vec<bool>,
    /// What this library exports, empty for anything that is not one.
    exports: NativeExportSurface,
    /// Which engine owns each function, in [`IrProgram::functions`] order.
    ///
    /// Resolved: no `Inherited` survives here, because a backend has to know
    /// where every function actually runs.
    engines: Vec<Execution>,
    /// The functions that are some type's user `Drop` body.
    ///
    /// This module carries one whatever engine owns it. A release happens
    /// wherever the value dies, and in a hybrid program native code releases
    /// values whose body the bytecode half also holds — so the body is compiled
    /// into both halves rather than reached across the bridge from a release
    /// leaf that has no frame to marshal from.
    drop_glue: BTreeSet<u32>,
    /// One entry per IR function, in [`IrProgram::functions`] order.
    ///
    /// Only functions this module defines have a real entry; a function that
    /// lives in the other half is reached through the bridge instead.
    functions: Vec<Option<Callable>>,
    /// One compact shared-graph descriptor per foreign import.
    foreign_ffi_descriptors: Vec<LLVMValueRef>,
    /// One compact shared-graph descriptor per foreign callback.
    callback_ffi_descriptors: Vec<LLVMValueRef>,
    /// The LLVM type of each declared struct, indexed by `StructId`.
    ///
    /// A struct lowers to a real LLVM struct with real field layout, not to a
    /// boxed or tagged value. Fields sit where the target's ABI puts them,
    /// which is what a `@FFI.Struct` will need — a box would be the wrong
    /// foundation to put underneath it later.
    struct_types: Vec<LLVMTypeRef>,
    /// Names every emitted string literal global uniquely.
    string_counter: u32,
    /// The host target's data layout, borrowed from the module.
    ///
    /// An array sizes its element by asking LLVM for the type's ABI size on
    /// this target (see [`Codegen::abi_size`]), so the stride matches what a
    /// struct field's offset would be — a guess computed here could disagree.
    /// Borrowed from the module, so it is valid as long as the module is and is
    /// never disposed here.
    target_data: LLVMTargetDataRef,
    /// The clone and free leaves an array's runtime helpers call, one per
    /// `(element type, leaf)`. Memoized so two arrays of the same element share
    /// one leaf. See [`elements`].
    element_leaves: HashMap<(Type, Leaf), LLVMValueRef>,
    /// The slots a copy or drop site hands its value to the leaf in, one pair
    /// per `(function, type)`. See [`glue`].
    scratch_slots: glue::ScratchSlots,
    /// The rebuild that carries one generic instantiation into another, one per
    /// `(from, to)`. `None` records a pair that needs no rebuild at all, so the
    /// answer is computed once either way. See [`widening`].
    widen_leaves: HashMap<(Type, Type), Option<Callable>>,
    /// Encode/decode leaves used by generic callback-state array conversion.
    native_state_leaves: HashMap<(Type, StateLeaf), LLVMValueRef>,
    /// Encode/decode helpers for payload-carrying enum callback state.
    native_state_enum_leaves: HashMap<(kira_semantics_model::EnumId, StateLeaf), Callable>,
    /// Optional debug builder for an explicitly requested debug build.
    debug: Option<debug::DebugBuilder>,
}

impl<'a> Codegen<'a> {
    /// Prepares the module scaffold: types, runtime declarations, and one
    /// declaration per Kira function.
    fn new(
        owned: &Module,
        program: &'a IrProgram,
        plan: Plan<'_>,
        debug_info: Option<&DebugInfo>,
    ) -> Result<Self, LlvmError> {
        let Plan {
            kind,
            engines,
            reachable,
            exports,
            pointer_width,
            target,
            unavailable,
            unit,
        } = plan;
        let types = Types::new(owned.context, pointer_width);
        let runtime = declare_runtime(owned.module, &types);

        // The module needs its target layout in place before any element is
        // sized, and object emission sets the same layout again harmlessly.
        let target_machine = match &target {
            CodegenTarget::Native(native) => TargetMachine::for_target(native, false, false)?,
            CodegenTarget::Wasm(device) => TargetMachine::wasm(*device)?,
        };
        target_machine.set_module_layout(owned.module);
        // SAFETY: the layout was just set, so the module has one; the returned
        // handle borrows it and lives as long as the module does.
        let target_data = unsafe { LLVMGetModuleDataLayout(owned.module) };

        let mut codegen = Codegen {
            program,
            target,
            unavailable: unavailable.to_vec(),
            context: owned.context,
            module: owned.module,
            builder: owned.builder,
            types,
            runtime,
            kind,
            unit,
            reachable,
            exports: exports.clone(),
            engines,
            drop_glue: program
                .types
                .structs()
                .defs()
                .iter()
                .filter_map(|def| def.drop_glue)
                .collect(),
            functions: Vec::with_capacity(program.functions.len()),
            foreign_ffi_descriptors: Vec::with_capacity(program.foreign_imports.len()),
            callback_ffi_descriptors: Vec::with_capacity(program.foreign_callbacks.len()),
            struct_types: Vec::with_capacity(program.types.structs().len()),
            string_counter: 0,
            target_data,
            element_leaves: HashMap::new(),
            scratch_slots: glue::ScratchSlots::new(),
            widen_leaves: HashMap::new(),
            native_state_leaves: HashMap::new(),
            native_state_enum_leaves: HashMap::new(),
            pointer_width,
            debug: debug_info.map(|info| {
                debug::DebugBuilder::new(owned.module, owned.context, owned.builder, info)
            }),
        };
        // Struct types come first: a function signature may name one, and a
        // struct's fields may name a struct declared before it.
        codegen.declare_structs()?;
        codegen.declare_foreign_ffi_descriptors()?;
        for (index, function) in program.functions.iter().enumerate() {
            // A function that runs on the other engine has no body here; its
            // callers reach it through the bridge, so there is nothing to
            // declare.
            let declared = if codegen.carries_body(index)
                && codegen.reachable.get(index).copied().unwrap_or(false)
            {
                Some(codegen.declare_function(index, function)?)
            } else {
                None
            };
            codegen.functions.push(declared);
        }
        Ok(codegen)
    }

    /// Creates one named LLVM struct type per declared struct, in two passes.
    ///
    /// A field may name a struct declared later: the analyzer's flat-package
    /// scope no longer makes declaration order resolution order, so a struct at
    /// a low id can hold one at a higher id. The first pass creates every named
    /// struct opaque, so the second — which sets bodies — finds every element
    /// type already in `struct_types`, whatever the order. A by-value value
    /// cycle cannot reach here: the analyzer breaks it to `Error`, so every body
    /// this sets is finitely sized. The types are named so LLVM IR stays
    /// readable (`%Point`, not `{i64, i64}`).
    fn declare_structs(&mut self) -> Result<(), LlvmError> {
        for def in self.program.types.structs().defs() {
            let name = c_string(&def.name);
            // SAFETY: the context is live and `name` outlives this call, which
            // only copies the string; the body is set in the second pass.
            let ty = unsafe { LLVMStructCreateNamed(self.context, name.as_ptr()) };
            self.struct_types.push(ty);
        }
        for index in 0..self.program.types.structs().len() {
            let mut fields = Vec::new();
            if let Some(def) = self.program.types.structs().defs().get(index) {
                fields.reserve(def.fields.len());
                for field in &def.fields {
                    fields.push(self.llvm_type(field.ty)?);
                }
            }
            let Some(&ty) = self.struct_types.get(index) else {
                continue;
            };
            // SAFETY: `ty` is a named struct in this context, every field type
            // belongs to it, and `fields` outlives the `SetBody` call that
            // copies it.
            unsafe {
                LLVMStructSetBody(ty, fields.as_mut_ptr(), fields.len() as u32, 0);
            }
        }
        Ok(())
    }

    /// Whether this module compiles function `index`'s body.
    ///
    /// The engine that owns it, plus every user `Drop` body whatever engine
    /// owns it: a release is emitted from the type rather than from a frame, so
    /// a release leaf here has no bridge to reach a body in the other half
    /// with. The body chose no engine — a `Drop` member may not be annotated —
    /// so compiling it into both halves runs the same source either way.
    fn carries_body(&self, index: usize) -> bool {
        self.engine_of(index) == Execution::Native || self.drop_glue.contains(&(index as u32))
    }

    /// Which engine owns function `index`.
    fn engine_of(&self, index: usize) -> Execution {
        self.engines
            .get(index)
            .copied()
            .unwrap_or(Execution::Runtime)
    }

    /// Declares one Kira function.
    fn declare_function(
        &mut self,
        index: usize,
        function: &IrFunction,
    ) -> Result<Callable, LlvmError> {
        let mut params = Vec::with_capacity(function.param_count as usize);
        for slot in 0..function.param_count {
            let ty = function
                .param_type(slot)
                .ok_or(LlvmError::internal("a function with a missing parameter"))?;
            // A written-through parameter — a mutating method's receiver, or one
            // declared `borrow mut` — is a pointer to the caller's storage, so a
            // write to it lands there and is observable after the call.
            //
            // A read-only `borrow` of something worth copying is a pointer too,
            // for the opposite reason: the caller keeps the value, so the callee
            // reads it where it lies rather than taking a duplicate of it. Every
            // other parameter is passed by value.
            if self.param_is_pointer(function, slot) {
                params.push(self.types.ptr);
            } else {
                params.push(self.llvm_type(ty)?);
            }
        }
        let return_type = self.llvm_type(function.return_type)?;
        let symbol = c_string(&symbol_name(index, &function.name));

        // SAFETY: `params` outlives the call, and every type is from this
        // module's context.
        let callable = unsafe {
            let ty = LLVMFunctionType(
                return_type,
                params.as_mut_ptr(),
                params.len() as u32,
                0, // not variadic
            );
            Callable {
                ty,
                value: LLVMAddFunction(self.module, symbol.as_ptr(), ty),
            }
        };
        Ok(callable)
    }

    /// Lowers every body this module owns, then whatever starts it.
    fn lower_program(&mut self) -> Result<(), LlvmError> {
        let program = self.program;
        for (index, function) in program.functions.iter().enumerate() {
            if !self.carries_body(index)
                || !self.reachable.get(index).copied().unwrap_or(false)
                || !self.unit.owns(index)
            {
                continue;
            }
            self.lower_function(index, function)?;
        }
        // One module owns the callback thunks. A later codegen unit keeps only
        // declarations, so callback addresses remain unique in a split build.
        if self.unit.is_first() {
            self.clear_debug_location();
            self.emit_foreign_callbacks()?;
        }
        if !self.unit.is_first() {
            return Ok(());
        }
        let result = match self.kind {
            // A whole program is entered through C `main`.
            ModuleKind::Executable => self.lower_entry_point(),
            // A whole native live program is entered through the runner's
            // fixed shared-library symbol.
            ModuleKind::NativeLiveLibrary => self.lower_native_live_entry_point(),
            // A library is entered by its consumer, so nothing starts it here.
            // Emitting a C `main` would make the artifact an executable that
            // happens to be a library, which is exactly the confusion the two
            // kinds exist to prevent. What a consumer reaches it through is the
            // export surface.
            ModuleKind::Library => self.lower_export_surface(),
            // A hybrid library is entered by its host, one call at a time.
            ModuleKind::HybridLibrary => {
                for (index, function) in program.functions.iter().enumerate() {
                    if self.engine_of(index) == Execution::Native
                        && self.reachable.get(index).copied().unwrap_or(false)
                        && self.unit.owns(index)
                    {
                        self.lower_trampoline(index, function)?;
                    }
                }
                Ok(())
            }
        };
        if result.is_ok()
            && let Some(debug) = self.debug.as_mut()
        {
            debug.finalize();
        }
        result
    }

    /// Attaches the current Kira source scope before a body is lowered.
    pub(super) fn begin_debug_function(&self, index: usize, value: LLVMValueRef) {
        let Some(debug) = self.debug.as_ref() else {
            return;
        };
        debug.attach(index, value);
        debug.set_location(index);
    }

    /// Prevents generated adapters and entry trampolines inheriting a Kira
    /// source line from the last lowered body.
    fn clear_debug_location(&self) {
        if let Some(debug) = self.debug.as_ref() {
            debug.clear_location();
        }
    }

    /// The LLVM type a Kira value type lowers to.
    /// How a borrowed parameter reaches a callee in this module.
    ///
    /// The read-only half is the same whole-program decision
    /// [`Codegen::param_is_pointer`] describes, said in the vocabulary
    /// `kira_ir::mid` plans releases with — so the stage that decides what a
    /// function releases is told the one thing about this module it cannot read
    /// off the function itself. The write-through half needs no such decision:
    /// a `borrow mut` is a pointer into the caller's frame in every native
    /// module, whatever shape it is.
    fn lending(&self) -> kira_ir::mid::Lending {
        kira_ir::mid::Lending {
            read_only: match self.kind {
                ModuleKind::Executable | ModuleKind::NativeLiveLibrary => {
                    kira_ir::mid::BorrowLending::ByPointer
                }
                _ => kira_ir::mid::BorrowLending::ByValue,
            },
            write_through: kira_ir::mid::BorrowLending::ByPointer,
            // Always lent, whatever the module kind: a copy of a value that
            // runs a user `Drop` is a second value with the same body to run,
            // and the release that runs it once would run it twice.
            user_drop: kira_ir::mid::BorrowLending::ByPointer,
        }
    }

    /// Whether parameter `slot` of `function` arrives as a pointer here.
    ///
    /// Lending a read-only borrow is a whole-program decision: every call to the
    /// function has to agree with its signature, so it is only done where this
    /// module compiles all of them. A hybrid half is called by the VM through a
    /// trampoline, a library by a consumer through its export surface, and a
    /// sidecar by a host — none of which knows about a pointer this module
    /// decided to use, so those keep passing by value.
    fn param_is_pointer(&self, function: &IrFunction, slot: u32) -> bool {
        if function.param_by_reference(slot) {
            return true;
        }
        if !function.param_by_pointer(slot) {
            return false;
        }
        // A borrow of a value that runs a user `Drop` is lent in every module
        // kind; see the `user_drop` field of `kira_ir::mid::Lending`.
        matches!(
            self.kind,
            ModuleKind::Executable | ModuleKind::NativeLiveLibrary
        ) || function
            .locals
            .get(slot as usize)
            .is_some_and(|&ty| self.program.types.runs_user_drop(ty))
    }

    fn llvm_type(&self, ty: Type) -> Result<LLVMTypeRef, LlvmError> {
        Ok(match ty {
            Type::Int(_) => self.types.i64,
            Type::Float(_) => self.types.f64,
            Type::Bool => self.types.i1,
            // A `String` is an opaque owned handle: one pointer the backend
            // never inspects, matching the runtime's ABI. An array and an enum
            // are the same shape and for the same reason — the runtime owns
            // their layout (an enum is a boxed tag plus its payload), and this
            // only ever passes the handle around.
            // `Any` joins them: an erased value is a handle to a box shaped like
            // the enum box — a tag saying what was erased, what the payload
            // word owns, and the word — so it is one opaque pointer here too.
            // A capture cell is the same shape again, and literally the same
            // box as an enum: `kira-native-bridge`'s `cells` module boxes a
            // held value in a `KiraEnum` with the tag unused.
            Type::String | Type::Array(_) | Type::Enum(_) | Type::Any | Type::Cell(_) => {
                self.types.ptr
            }
            // A `RawPtr` is an opaque target-width word Kira only stores and
            // passes back; it is represented as an `i64` payload and never
            // dereferenced, exactly as the VM keeps it in a `Value::RawPtr`.
            // Foreign marshalling converts it to a real pointer at the C
            // boundary inside the generated adapter.
            // A `CString` is a pointer word too. It names a foreign parameter
            // position, where it never becomes a value — but as a *member* of a
            // C-layout struct it is real storage: the address of bytes that
            // outlive the call, which Kira stores and passes back and never
            // dereferences.
            Type::RawPtr
            | Type::ForeignPtr(_)
            | Type::NativeState(_)
            | Type::CString
            | Type::Task(_)
            | Type::CBlock => self.types.i64,
            Type::Void => self.types.void,
            Type::Struct(id) => *self
                .struct_types
                .get(id.index() as usize)
                .ok_or(LlvmError::internal("a struct the module never declared"))?,
            // Lowering only ever runs on a program that type-checked, so an
            // error type here means a broken frontend contract, not user input.
            Type::Error => {
                return Err(LlvmError::internal("a program that failed to type-check"));
            }
        })
    }

    /// Emits a call to a runtime helper.
    ///
    /// A call that returns nothing is emitted unnamed whatever `name` says: an
    /// LLVM instruction producing `void` may not carry one, and naming it is a
    /// module the verifier rejects with "instruction has a name, but provides a
    /// void value". Deciding it here rather than at each call site means a
    /// helper that gains or loses a result cannot leave a caller emitting an
    /// invalid module.
    ///
    /// # Safety
    /// The builder must be positioned at the end of a block, and `args` must
    /// match `callable`'s signature.
    unsafe fn call_runtime(
        &self,
        callable: Callable,
        args: &mut [LLVMValueRef],
        name: &CStr,
    ) -> LLVMValueRef {
        // SAFETY: the caller positions the builder and supplies matching
        // arguments; `args` outlives the call. `callable.ty` is the callee's
        // function type, so its return type is live for the query.
        unsafe {
            let returns_void =
                LLVMGetTypeKind(LLVMGetReturnType(callable.ty)) == LLVMTypeKind::LLVMVoidTypeKind;
            let name = if returns_void { c"" } else { name };
            LLVMBuildCall2(
                self.builder,
                callable.ty,
                callable.value,
                args.as_mut_ptr(),
                args.len() as u32,
                name.as_ptr(),
            )
        }
    }
}
