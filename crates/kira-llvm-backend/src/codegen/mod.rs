//! Lowering a verified [`IrProgram`] into an LLVM module, and emitting it as a
//! native object.
//!
//! This module owns the LLVM objects (context, module, builder) and the
//! program-wide scaffold: the type mapping, the declarations of the
//! `kira_rt_*` runtime helpers, one declaration per Kira function, and the C
//! `main` that starts the program. Statement and expression lowering lives in
//! [`lower`].
//!
//! Every LLVM object here is a raw pointer from the C API, so the whole module
//! is one `unsafe` fence: [`Module`] owns its context and disposes of it on
//! drop, and no LLVM reference escapes that lifetime.

mod lower;

use std::ffi::{CStr, CString};
use std::path::Path;

use kira_ir::{IrFunction, IrProgram};
use kira_runtime_abi::{BridgeValueTag, Execution};
use kira_semantics_model::Type;
use llvm_sys::analysis::{LLVMVerifierFailureAction, LLVMVerifyModule};
use llvm_sys::core::*;
use llvm_sys::prelude::*;
use llvm_sys::target::{
    LLVM_InitializeNativeAsmPrinter, LLVM_InitializeNativeTarget, LLVMDisposeTargetData,
    LLVMSetModuleDataLayout,
};
use llvm_sys::target_machine::*;

use crate::LlvmError;

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
}

/// A callable LLVM value together with its function type.
///
/// Opaque pointers mean a function value no longer carries its signature, so
/// every call site needs the type back; keeping them paired makes that
/// impossible to get wrong.
#[derive(Clone, Copy)]
pub(crate) struct Callable {
    /// The function's type.
    ty: LLVMTypeRef,
    /// The function value.
    value: LLVMValueRef,
}

/// The LLVM types Kira's v0 value types map onto.
#[derive(Clone, Copy)]
pub(crate) struct Types {
    void: LLVMTypeRef,
    i1: LLVMTypeRef,
    i8: LLVMTypeRef,
    i32: LLVMTypeRef,
    i64: LLVMTypeRef,
    f64: LLVMTypeRef,
    /// The opaque pointer every `String` handle is.
    ptr: LLVMTypeRef,
    /// `BridgeValue`: `{ i8 tag, [7 x i8] reserved, i64 payload }`.
    ///
    /// Mirrors `kira_runtime_abi::BridgeValue` exactly; that crate's layout test
    /// is what pins the shape this must agree with.
    bridge_value: LLVMTypeRef,
}

/// The `kira_rt_*` runtime helpers, declared once per module.
///
/// These names are the wire contract with `kira-native-bridge`; they are
/// append-only and must match its `extern "C"` signatures exactly.
#[derive(Clone, Copy)]
pub(crate) struct Runtime {
    print_int: Callable,
    print_float: Callable,
    print_bool: Callable,
    print_str: Callable,
    str_new: Callable,
    str_clone: Callable,
    str_concat: Callable,
    str_eq: Callable,
    str_free: Callable,
    trap_div_zero: Callable,
    /// The version marker every emitted program references; see
    /// [`kira_runtime_abi::RUNTIME_ABI_MARKER`].
    abi_marker: Callable,
    /// `kira_hybrid_call_runtime`: how native code reaches the VM half.
    call_runtime: Callable,
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

/// The host target machine, used to emit objects.
struct TargetMachine {
    machine: LLVMTargetMachineRef,
    triple: *mut std::os::raw::c_char,
}

impl TargetMachine {
    /// Builds a target machine for the compiling host.
    fn host() -> Result<Self, LlvmError> {
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
    fn emit_object(&self, module: LLVMModuleRef, path: &Path) -> Result<(), LlvmError> {
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
    /// Names every emitted string literal global uniquely.
    string_counter: u32,
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
        // SAFETY: every type below is created in this module's live context.
        let types = unsafe {
            Types {
                void: LLVMVoidTypeInContext(owned.context),
                i1: LLVMInt1TypeInContext(owned.context),
                i8: LLVMInt8TypeInContext(owned.context),
                i32: LLVMInt32TypeInContext(owned.context),
                i64: LLVMInt64TypeInContext(owned.context),
                f64: LLVMDoubleTypeInContext(owned.context),
                ptr: LLVMPointerTypeInContext(owned.context, 0),
                bridge_value: bridge_value_type(owned.context),
            }
        };
        let runtime = declare_runtime(owned.module, &types);

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
            string_counter: 0,
        };
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

    /// Which engine owns function `index`.
    fn engine_of(&self, index: usize) -> Execution {
        self.engines
            .get(index)
            .copied()
            .unwrap_or(Execution::Runtime)
    }

    /// Declares one Kira function.
    ///
    /// Symbols are `kira_fn_<index>_<name>`: the index makes every symbol unique
    /// even when two Kira functions share a name, and keeps the symbol stable
    /// against source reordering within a program.
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

    /// Emits the C `main` that runs the program.
    ///
    /// It calls `@Main` and exits 0, mirroring the CLI's VM path: the VM
    /// discards the entrypoint's result and reports success, so native does the
    /// same — freeing the result first when it owns a string, exactly as the VM
    /// drops it.
    fn lower_entry_point(&mut self) -> Result<(), LlvmError> {
        let entry = self.functions[self.program.main as usize]
            .ok_or(LlvmError::Unsupported("an entrypoint with no native body"))?;
        let main_function = self.program.main_function();

        // SAFETY: every value and type below belongs to this live module, and
        // the builder is positioned on a block of the function being built.
        unsafe {
            let main_ty = LLVMFunctionType(self.types.i32, std::ptr::null_mut(), 0, 0);
            let main = LLVMAddFunction(self.module, c"main".as_ptr(), main_ty);
            let block = LLVMAppendBasicBlockInContext(self.context, main, c"entry".as_ptr());
            LLVMPositionBuilderAtEnd(self.builder, block);

            // Reference the runtime's ABI marker before anything else. The call
            // is empty and free; emitting it is what makes a runtime archive
            // built against a different `kira_rt_*` contract fail to link by
            // name, instead of resolving the old code under the new ABI and
            // corrupting memory at run time.
            self.call_runtime(self.runtime.abi_marker, &mut [], c"");

            let name = if main_function.return_type == Type::Void {
                c"".as_ptr()
            } else {
                c"kira.main.result".as_ptr()
            };
            let result = LLVMBuildCall2(
                self.builder,
                entry.ty,
                entry.value,
                std::ptr::null_mut(),
                0,
                name,
            );
            if main_function.return_type == Type::String {
                self.call_runtime(self.runtime.str_free, &mut [result], c"");
            }
            LLVMBuildRet(self.builder, LLVMConstInt(self.types.i32, 0, 0));
        }
        Ok(())
    }

    /// Emits the trampoline the host calls to reach native function `index`.
    ///
    /// ```text
    /// void kira_native_fn_<id>(const BridgeValue *args, u32 count, BridgeValue *out)
    /// ```
    ///
    /// One C-ABI shape for every Kira signature, so the host can call any native
    /// function through one function-pointer type rather than needing a
    /// per-signature thunk. The trampoline unpacks each argument to the type the
    /// manifest promised, calls the real body, and packs the result back.
    ///
    /// `count` is not checked against the signature: the host builds the call
    /// from the same manifest this was generated from, so a mismatch is a broken
    /// artifact rather than a runtime condition — and the manifest's decoder is
    /// where artifacts are validated.
    fn lower_trampoline(&mut self, index: usize, function: &IrFunction) -> Result<(), LlvmError> {
        let target = self.functions[index].ok_or(LlvmError::Unsupported(
            "a trampoline to a function with no body",
        ))?;
        let symbol = c_string(&trampoline_name(index));
        let types = self.types;

        // SAFETY: every type and value below belongs to this live module, and
        // the builder is positioned on the trampoline's own block before any
        // instruction is built.
        unsafe {
            let mut params = [types.ptr, types.i32, types.ptr];
            let signature =
                LLVMFunctionType(types.void, params.as_mut_ptr(), params.len() as u32, 0);
            let trampoline = LLVMAddFunction(self.module, symbol.as_ptr(), signature);
            let block = LLVMAppendBasicBlockInContext(self.context, trampoline, c"entry".as_ptr());
            LLVMPositionBuilderAtEnd(self.builder, block);

            let args = LLVMGetParam(trampoline, 0);
            let out = LLVMGetParam(trampoline, 2);

            let mut lowered = Vec::with_capacity(function.param_count as usize);
            for slot in 0..function.param_count {
                let ty = function
                    .param_type(slot)
                    .ok_or(LlvmError::Unsupported("a parameter with no type"))?;
                let mut offset = [LLVMConstInt(types.i32, u64::from(slot), 0)];
                let element = LLVMBuildInBoundsGEP2(
                    self.builder,
                    types.bridge_value,
                    args,
                    offset.as_mut_ptr(),
                    1,
                    c"arg.slot".as_ptr(),
                );
                lowered.push(self.read_bridge_payload(element, ty)?);
            }

            let returns_value = function.return_type != Type::Void;
            let name = if returns_value { c"result" } else { c"" };
            let result = LLVMBuildCall2(
                self.builder,
                target.ty,
                target.value,
                lowered.as_mut_ptr(),
                lowered.len() as u32,
                name.as_ptr(),
            );
            self.write_bridge_value(out, result, function.return_type)?;
            LLVMBuildRetVoid(self.builder);
        }
        Ok(())
    }

    /// Reads one `BridgeValue`'s payload as a value of type `ty`.
    ///
    /// The tag is not consulted: the static type is what the manifest promised
    /// and what the other side encoded from. The tag exists so a *reader* that
    /// does not know the signature can still refuse an unknown value.
    fn read_bridge_payload(&self, slot: LLVMValueRef, ty: Type) -> Result<LLVMValueRef, LlvmError> {
        let types = self.types;
        // SAFETY: `slot` points at a `BridgeValue` the caller supplied, and the
        // builder is on a live block.
        unsafe {
            let payload_ptr = LLVMBuildStructGEP2(
                self.builder,
                types.bridge_value,
                slot,
                2,
                c"arg.payload.ptr".as_ptr(),
            );
            let payload = LLVMBuildLoad2(
                self.builder,
                types.i64,
                payload_ptr,
                c"arg.payload".as_ptr(),
            );
            Ok(match ty {
                Type::Int => payload,
                Type::Float => {
                    LLVMBuildBitCast(self.builder, payload, types.f64, c"arg.float".as_ptr())
                }
                Type::Bool => LLVMBuildTrunc(self.builder, payload, types.i1, c"arg.bool".as_ptr()),
                Type::String => {
                    LLVMBuildIntToPtr(self.builder, payload, types.ptr, c"arg.str".as_ptr())
                }
                Type::Void | Type::Error => {
                    return Err(LlvmError::Unsupported("a parameter with no runtime value"));
                }
            })
        }
    }

    /// Writes `value` into the `BridgeValue` at `slot`, tagged for `ty`.
    fn write_bridge_value(
        &self,
        slot: LLVMValueRef,
        value: LLVMValueRef,
        ty: Type,
    ) -> Result<(), LlvmError> {
        let types = self.types;
        let (tag, payload) = bridge_tag_of(ty)?;
        // SAFETY: `slot` points at a writable `BridgeValue`, and the builder is
        // on a live block.
        unsafe {
            let payload = match payload {
                // Void carries no payload; zero keeps the reserved word defined.
                None => LLVMConstInt(types.i64, 0, 0),
                Some(PayloadForm::AsIs) => value,
                Some(PayloadForm::FloatBits) => {
                    LLVMBuildBitCast(self.builder, value, types.i64, c"ret.bits".as_ptr())
                }
                Some(PayloadForm::Widen) => {
                    LLVMBuildZExt(self.builder, value, types.i64, c"ret.wide".as_ptr())
                }
                Some(PayloadForm::PointerBits) => {
                    LLVMBuildPtrToInt(self.builder, value, types.i64, c"ret.handle".as_ptr())
                }
            };
            let tag_ptr = LLVMBuildStructGEP2(
                self.builder,
                types.bridge_value,
                slot,
                0,
                c"ret.tag.ptr".as_ptr(),
            );
            LLVMBuildStore(
                self.builder,
                LLVMConstInt(types.i8, u64::from(tag), 0),
                tag_ptr,
            );
            let payload_ptr = LLVMBuildStructGEP2(
                self.builder,
                types.bridge_value,
                slot,
                2,
                c"ret.payload.ptr".as_ptr(),
            );
            LLVMBuildStore(self.builder, payload, payload_ptr);
        }
        Ok(())
    }

    /// The LLVM type a Kira value type lowers to.
    fn llvm_type(&self, ty: Type) -> Result<LLVMTypeRef, LlvmError> {
        Ok(match ty {
            Type::Int => self.types.i64,
            Type::Float => self.types.f64,
            Type::Bool => self.types.i1,
            // A `String` is an opaque owned handle: one pointer the backend
            // never inspects, matching the runtime's ABI.
            Type::String => self.types.ptr,
            Type::Void => self.types.void,
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

/// The LLVM form of `kira_runtime_abi::BridgeValue`.
///
/// `{ i8, [7 x i8], i64 }` — the same 16 bytes, with the reserved gap spelled
/// out rather than left to the compiler, so this and the Rust struct cannot
/// disagree about where the payload sits.
fn bridge_value_type(context: LLVMContextRef) -> LLVMTypeRef {
    // SAFETY: every type is created in this live context; `fields` outlives the
    // struct-type call.
    unsafe {
        let i8_ty = LLVMInt8TypeInContext(context);
        let mut fields = [
            i8_ty,
            LLVMArrayType2(i8_ty, 7),
            LLVMInt64TypeInContext(context),
        ];
        LLVMStructTypeInContext(context, fields.as_mut_ptr(), fields.len() as u32, 0)
    }
}

/// Declares the `kira_rt_*` helpers the lowering calls.
fn declare_runtime(module: LLVMModuleRef, types: &Types) -> Runtime {
    // SAFETY: every type belongs to this module's context, and each parameter
    // slice outlives its `LLVMFunctionType` call.
    unsafe {
        let declare = |name: &CStr, ret: LLVMTypeRef, params: &mut [LLVMTypeRef]| -> Callable {
            let ty = LLVMFunctionType(ret, params.as_mut_ptr(), params.len() as u32, 0);
            Callable {
                ty,
                value: LLVMAddFunction(module, name.as_ptr(), ty),
            }
        };
        Runtime {
            print_int: declare(c"kira_rt_print_int", types.void, &mut [types.i64]),
            print_float: declare(c"kira_rt_print_float", types.void, &mut [types.f64]),
            print_bool: declare(c"kira_rt_print_bool", types.void, &mut [types.i8]),
            print_str: declare(c"kira_rt_print_str", types.void, &mut [types.ptr]),
            str_new: declare(c"kira_rt_str_new", types.ptr, &mut [types.ptr, types.i64]),
            str_clone: declare(c"kira_rt_str_clone", types.ptr, &mut [types.ptr]),
            str_concat: declare(
                c"kira_rt_str_concat",
                types.ptr,
                &mut [types.ptr, types.ptr],
            ),
            str_eq: declare(c"kira_rt_str_eq", types.i8, &mut [types.ptr, types.ptr]),
            str_free: declare(c"kira_rt_str_free", types.void, &mut [types.ptr]),
            trap_div_zero: declare(c"kira_rt_trap_div_zero", types.void, &mut []),
            abi_marker: declare(&abi_marker_symbol(), types.void, &mut []),
            call_runtime: declare(
                c"kira_hybrid_call_runtime",
                types.void,
                // (function_id, args, count, out)
                &mut [types.i32, types.ptr, types.i32, types.ptr],
            ),
        }
    }
}

/// The runtime ABI marker's symbol, as a C string.
///
/// Built from the shared constant rather than spelled here, so the backend and
/// the runtime archive cannot drift apart silently.
fn abi_marker_symbol() -> CString {
    c_string(kira_runtime_abi::RUNTIME_ABI_MARKER)
}

/// How a value of type `ty` sits in a `BridgeValue` payload.
enum PayloadForm {
    /// Already an `i64`.
    AsIs,
    /// A `double` reinterpreted as bits.
    FloatBits,
    /// A narrower integer widened.
    Widen,
    /// A pointer as an integer.
    PointerBits,
}

/// The bridge tag for `ty`, and how its payload is encoded.
fn bridge_tag_of(ty: Type) -> Result<(u8, Option<PayloadForm>), LlvmError> {
    Ok(match ty {
        Type::Void => (BridgeValueTag::VOID.0, None),
        Type::Int => (BridgeValueTag::INT.0, Some(PayloadForm::AsIs)),
        Type::Float => (BridgeValueTag::FLOAT.0, Some(PayloadForm::FloatBits)),
        Type::Bool => (BridgeValueTag::BOOL.0, Some(PayloadForm::Widen)),
        Type::String => (BridgeValueTag::STRING.0, Some(PayloadForm::PointerBits)),
        Type::Error => return Err(LlvmError::Unsupported("a value with no type")),
    })
}

/// The symbol of the trampoline the host calls to reach native function `index`.
///
/// A wire contract with the hybrid manifest, which records this name as the
/// function's exported symbol.
pub(crate) fn trampoline_name(index: usize) -> String {
    format!("kira_native_fn_{index}")
}

/// The native symbol for Kira function `index`.
fn symbol_name(index: usize, name: &str) -> String {
    let sanitized: String = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect();
    format!("kira_fn_{index}_{sanitized}")
}

/// Builds a NUL-terminated copy of `text` for the C API.
///
/// Interior NUL bytes are replaced rather than rejected: these strings are
/// module and symbol names, where a NUL cannot carry meaning, and a name is
/// never a reason to fail a build.
fn c_string(text: &str) -> CString {
    CString::new(text.replace('\0', "_")).unwrap_or_else(|_| CString::default())
}

/// Takes ownership of an LLVM-allocated message, returning it as a `String`.
///
/// # Safety
/// `message` must be null or a NUL-terminated string LLVM allocated.
unsafe fn take_message(message: *mut std::os::raw::c_char) -> String {
    if message.is_null() {
        return "LLVM reported no detail".to_owned();
    }
    // SAFETY: the caller guarantees a NUL-terminated LLVM-allocated string;
    // the text is copied out before the allocation is released.
    unsafe {
        let text = CStr::from_ptr(message).to_string_lossy().into_owned();
        LLVMDisposeMessage(message);
        text
    }
}

/// Releases an LLVM-allocated message, if any.
///
/// # Safety
/// `message` must be null or a string LLVM allocated.
unsafe fn dispose_message(message: *mut std::os::raw::c_char) {
    if !message.is_null() {
        // SAFETY: the caller guarantees an LLVM-allocated string.
        unsafe { LLVMDisposeMessage(message) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbols_are_unique_per_function_and_sanitized() {
        assert_eq!(symbol_name(0, "main"), "kira_fn_0_main");
        assert_eq!(symbol_name(3, "fib"), "kira_fn_3_fib");
        // Two functions sharing a name never collide on a symbol.
        assert_ne!(symbol_name(1, "helper"), symbol_name(2, "helper"));
        // Anything a linker could not carry is replaced, not passed through.
        assert_eq!(symbol_name(0, "odd name!"), "kira_fn_0_odd_name_");
    }

    #[test]
    fn names_with_interior_nuls_still_build_a_c_string() {
        assert_eq!(c_string("a\0b").to_bytes(), b"a_b");
    }
}
