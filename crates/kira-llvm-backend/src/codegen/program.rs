//! Program-wide codegen state and the scaffold shared by every body.

use std::collections::HashMap;
use std::ffi::CStr;

use kira_debug::DebugInfo;
use kira_ir::{IrFunction, IrProgram};
use kira_runtime_abi::Execution;
use kira_semantics_model::Type;
use llvm_sys::LLVMTypeKind;
use llvm_sys::core::*;
use llvm_sys::prelude::*;
use llvm_sys::target::LLVMGetModuleDataLayout;

use super::ffi::c_string;
use super::symbols::symbol_name;
use super::target::TargetMachine;
use super::types::{Callable, Types, declare_runtime};
use super::{Codegen, CodegenTarget, Module, ModuleKind, Plan};
use super::{debug, glue};
use crate::LlvmError;

impl<'a> Codegen<'a> {
    /// Prepares the module scaffold: types, runtime declarations, and one
    /// declaration per Kira function.
    pub(super) fn new(
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
            lifecycle_functions: crate::reachability::lifecycle_functions(program),
            drop_glue: program
                .types
                .structs()
                .defs()
                .iter()
                .filter_map(|def| def.drop_glue)
                .collect(),
            functions: Vec::with_capacity(program.functions.len()),
            constant_globals: Vec::with_capacity(program.constants.len()),
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
        codegen.declare_constant_globals()?;
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
    pub(super) fn declare_structs(&mut self) -> Result<(), LlvmError> {
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
    pub(super) fn carries_body(&self, index: usize) -> bool {
        self.engine_of(index) == Execution::Native || self.drop_glue.contains(&(index as u32))
    }

    /// Which engine owns function `index`.
    pub(super) fn engine_of(&self, index: usize) -> Execution {
        self.engines
            .get(index)
            .copied()
            .unwrap_or(Execution::Runtime)
    }

    /// Declares one Kira function.
    pub(super) fn declare_function(
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
    pub(super) fn lower_program(&mut self) -> Result<(), LlvmError> {
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
        // A main-thread target needs a bridge-shaped entry even when no hybrid
        // host is involved. In a split native build the target's owning unit
        // emits the body and the first unit's dispatcher links to this symbol;
        // in a whole module both happen here.
        for (index, function) in program.functions.iter().enumerate() {
            if function.is_main_thread
                && self.engine_of(index) == Execution::Native
                && self.carries_body(index)
                && self.reachable.get(index).copied().unwrap_or(false)
                && self.unit.owns(index)
            {
                self.lower_trampoline(index, function)?;
            }
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
                self.lower_main_thread_dispatcher()?;
                self.lower_main_thread_lifecycle_resolver()?;
                for (index, function) in program.functions.iter().enumerate() {
                    if self.engine_of(index) == Execution::Native
                        && !function.is_main_thread
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
    pub(super) fn clear_debug_location(&self) {
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
    pub(super) fn lending(&self) -> kira_ir::mid::Lending {
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
    pub(super) fn param_is_pointer(&self, function: &IrFunction, slot: u32) -> bool {
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

    pub(super) fn llvm_type(&self, ty: Type) -> Result<LLVMTypeRef, LlvmError> {
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
            | Type::MainThreadTask(_)
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
    pub(super) unsafe fn call_runtime(
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
