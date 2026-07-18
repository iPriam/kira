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
//! - [`target`] — the host target machine and object emission,
//! - [`ffi`] — glue for the LLVM C API's strings.
//!
//! Every LLVM object here is a raw pointer from the C API, so the whole module
//! is one `unsafe` fence: [`Module`] owns its context and disposes of it on
//! drop, and no LLVM reference escapes that lifetime.

mod bridge;
mod elements;
mod entry;
mod ffi;
mod lower;
mod symbols;
mod target;
mod types;
mod values;

use std::collections::HashMap;
use std::ffi::CStr;
use std::path::Path;

use kira_ir::{IrFunction, IrProgram};
use kira_runtime_abi::Execution;
use kira_semantics_model::Type;
use llvm_sys::analysis::{LLVMVerifierFailureAction, LLVMVerifyModule};
use llvm_sys::core::*;
use llvm_sys::prelude::*;
use llvm_sys::target::{LLVMGetModuleDataLayout, LLVMTargetDataRef};

use self::elements::Leaf;

use self::ffi::{c_string, dispose_message, take_message};
use self::symbols::symbol_name;
use self::target::TargetMachine;
use self::types::{Runtime, Types, declare_runtime};
use crate::LlvmError;

pub(crate) use self::symbols::trampoline_name;
pub(crate) use self::types::Callable;

/// What this module is being built as.
///
/// The two modes differ only in which functions have bodies here and how the
/// program is entered, so they share one lowering with an engine plan rather
/// than duplicating it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModuleKind {
    /// A whole program: every function is native and a C `main` starts it.
    Executable,
    /// The native half of a hybrid program: only `@Native` functions have
    /// bodies, each also gets a trampoline the host can call, and there is no
    /// `main` — the host is the program.
    HybridLibrary,
    /// A whole Kira library: every function is native, exactly as in an
    /// [`ModuleKind::Executable`], and no C `main` is emitted because a library
    /// is entered by its consumer rather than started by the operating system.
    ///
    /// No trampolines yet either. Which functions a consumer may call, and
    /// under what symbol, is what `@Export` decides, and that is a later step;
    /// a library today is a linkable artifact carrying every function's code
    /// and no entry point.
    Library,
}

/// An LLVM module holding a lowered Kira program.
///
/// Owns its LLVM context; dropping it disposes of every LLVM object built from
/// it, which is why no reference into the module outlives this value.
pub(crate) struct Module {
    context: LLVMContextRef,
    module: LLVMModuleRef,
    builder: LLVMBuilderRef,
}

impl Module {
    /// Lowers a whole program into an LLVM module for a native executable.
    ///
    /// Every function is native here, whatever it was annotated: a whole-program
    /// native build has no VM half, so `@Runtime` marks a boundary this build
    /// does not have. That is the mirror of the VM-only build compiling every
    /// function to bytecode, and it is what keeps the two backends agreeing on
    /// any program.
    pub(crate) fn build(program: &IrProgram, module_name: &str) -> Result<Self, LlvmError> {
        let engines = vec![Execution::Native; program.functions.len()];
        Self::lower(program, module_name, ModuleKind::Executable, engines)
    }

    /// Lowers a whole Kira library into an LLVM module with no entry point.
    ///
    /// Every function is native, exactly as in [`Module::build`]: the two
    /// differ only in whether a C `main` is emitted, which keeps a library and
    /// a program compiling their shared code through one path.
    pub(crate) fn build_library(program: &IrProgram, module_name: &str) -> Result<Self, LlvmError> {
        let engines = vec![Execution::Native; program.functions.len()];
        Self::lower(program, module_name, ModuleKind::Library, engines)
    }

    /// Lowers the native half of a hybrid program into a shared library.
    pub(crate) fn build_hybrid(program: &IrProgram, module_name: &str) -> Result<Self, LlvmError> {
        let engines = program
            .functions
            .iter()
            .map(|function| function.execution.resolve(Execution::Runtime))
            .collect();
        Self::lower(program, module_name, ModuleKind::HybridLibrary, engines)
    }

    /// Builds the module.
    fn lower(
        program: &IrProgram,
        module_name: &str,
        kind: ModuleKind,
        engines: Vec<Execution>,
    ) -> Result<Self, LlvmError> {
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
            }
        };

        let mut codegen = Codegen::new(&owned, program, kind, engines)?;
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

    /// Emits a native object file for the host into `path`.
    pub(crate) fn emit_object(&self, path: &Path) -> Result<(), LlvmError> {
        let machine = TargetMachine::host()?;
        machine.emit_object(self.module, path)
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
    context: LLVMContextRef,
    module: LLVMModuleRef,
    builder: LLVMBuilderRef,
    types: Types,
    runtime: Runtime,
    /// What this module is being built as.
    kind: ModuleKind,
    /// Which engine owns each function, in [`IrProgram::functions`] order.
    ///
    /// Resolved: no `Inherited` survives here, because a backend has to know
    /// where every function actually runs.
    engines: Vec<Execution>,
    /// One entry per IR function, in [`IrProgram::functions`] order.
    ///
    /// Only functions this module defines have a real entry; a function that
    /// lives in the other half is reached through the bridge instead.
    functions: Vec<Option<Callable>>,
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
}

impl<'a> Codegen<'a> {
    /// Prepares the module scaffold: types, runtime declarations, and one
    /// declaration per Kira function.
    fn new(
        owned: &Module,
        program: &'a IrProgram,
        kind: ModuleKind,
        engines: Vec<Execution>,
    ) -> Result<Self, LlvmError> {
        let types = Types::new(owned.context);
        let runtime = declare_runtime(owned.module, &types);

        // The module needs the host's data layout in place before any element
        // is sized, and object emission sets the same layout again (harmlessly)
        // when it runs. `target_data` borrows it from the module.
        TargetMachine::host()?.set_module_layout(owned.module);
        // SAFETY: the layout was just set, so the module has one; the returned
        // handle borrows it and lives as long as the module does.
        let target_data = unsafe { LLVMGetModuleDataLayout(owned.module) };

        let mut codegen = Codegen {
            program,
            context: owned.context,
            module: owned.module,
            builder: owned.builder,
            types,
            runtime,
            kind,
            engines,
            functions: Vec::with_capacity(program.functions.len()),
            struct_types: Vec::with_capacity(program.types.structs().len()),
            string_counter: 0,
            target_data,
            element_leaves: HashMap::new(),
        };
        // Struct types come first: a function signature may name one, and a
        // struct's fields may name a struct declared before it.
        codegen.declare_structs()?;
        for (index, function) in program.functions.iter().enumerate() {
            // A function that runs on the other engine has no body here; its
            // callers reach it through the bridge, so there is nothing to
            // declare.
            let declared = if codegen.engine_of(index) == Execution::Native {
                Some(codegen.declare_function(index, function)?)
            } else {
                None
            };
            codegen.functions.push(declared);
        }
        Ok(codegen)
    }

    /// Creates one named LLVM struct type per declared struct.
    ///
    /// Declaration order is resolution order — the analyzer rejects a field
    /// naming a struct declared later — so every field type this needs is
    /// already in `struct_types` by the time it is asked for. The types are
    /// named so LLVM IR stays readable (`%Point`, not `{i64, i64}`).
    fn declare_structs(&mut self) -> Result<(), LlvmError> {
        for def in self.program.types.structs().defs() {
            let name = c_string(&def.name);
            let mut fields = Vec::with_capacity(def.fields.len());
            for field in &def.fields {
                fields.push(self.llvm_type(field.ty)?);
            }
            // SAFETY: the context is live, every field type belongs to it, and
            // `fields` outlives the `SetBody` call that copies it.
            let ty = unsafe {
                let ty = LLVMStructCreateNamed(self.context, name.as_ptr());
                LLVMStructSetBody(ty, fields.as_mut_ptr(), fields.len() as u32, 0);
                ty
            };
            self.struct_types.push(ty);
        }
        Ok(())
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
            let ty = function.param_type(slot).ok_or(LlvmError::Unsupported(
                "a function with a missing parameter",
            ))?;
            params.push(self.llvm_type(ty)?);
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
            if self.engine_of(index) != Execution::Native {
                continue;
            }
            self.lower_function(index, function)?;
        }
        match self.kind {
            // A whole program is entered through C `main`.
            ModuleKind::Executable => self.lower_entry_point(),
            // A library is entered by its consumer, so nothing starts it here.
            // Emitting a C `main` would make the artifact an executable that
            // happens to be a library, which is exactly the confusion the two
            // kinds exist to prevent.
            ModuleKind::Library => Ok(()),
            // A hybrid library is entered by its host, one call at a time.
            ModuleKind::HybridLibrary => {
                for (index, function) in program.functions.iter().enumerate() {
                    if self.engine_of(index) == Execution::Native {
                        self.lower_trampoline(index, function)?;
                    }
                }
                Ok(())
            }
        }
    }

    /// The LLVM type a Kira value type lowers to.
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
            Type::String | Type::Array(_) | Type::Enum(_) => self.types.ptr,
            Type::Void => self.types.void,
            Type::Struct(id) => *self
                .struct_types
                .get(id.index() as usize)
                .ok_or(LlvmError::Unsupported("a struct the module never declared"))?,
            // Lowering only ever runs on a program that type-checked, so an
            // error type here means a broken frontend contract, not user input.
            Type::Error => {
                return Err(LlvmError::Unsupported(
                    "a program that failed to type-check",
                ));
            }
        })
    }

    /// Emits a call to a runtime helper.
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
        // arguments; `args` outlives the call.
        unsafe {
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
