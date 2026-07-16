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
    /// Lowers `program` into a fresh LLVM module named `module_name`.
    pub(crate) fn build(program: &IrProgram, module_name: &str) -> Result<Self, LlvmError> {
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

        let mut codegen = Codegen::new(&owned, program)?;
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
    /// One entry per IR function, in [`IrProgram::functions`] order.
    functions: Vec<Callable>,
    /// Names every emitted string literal global uniquely.
    string_counter: u32,
}

impl<'a> Codegen<'a> {
    /// Prepares the module scaffold: types, runtime declarations, and one
    /// declaration per Kira function.
    fn new(owned: &Module, program: &'a IrProgram) -> Result<Self, LlvmError> {
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
            functions: Vec::with_capacity(program.functions.len()),
            string_counter: 0,
        };
        for (index, function) in program.functions.iter().enumerate() {
            let declared = codegen.declare_function(index, function)?;
            codegen.functions.push(declared);
        }
        Ok(codegen)
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

    /// Lowers every function body, then the C entry point.
    fn lower_program(&mut self) -> Result<(), LlvmError> {
        for (index, function) in self.program.functions.iter().enumerate() {
            self.lower_function(index, function)?;
        }
        self.lower_entry_point()
    }

    /// Emits the C `main` that runs the program.
    ///
    /// It calls `@Main` and exits 0, mirroring the CLI's VM path: the VM
    /// discards the entrypoint's result and reports success, so native does the
    /// same — freeing the result first when it owns a string, exactly as the VM
    /// drops it.
    fn lower_entry_point(&mut self) -> Result<(), LlvmError> {
        let entry = self.functions[self.program.main as usize];
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
