//! Lowering one function body: the part that must agree with the interpreter
//! instruction for instruction.
//!
//! Where LLVM's natural choice differs from the VM's, the VM wins:
//!
//! - `add`/`sub`/`mul` carry no `nsw`/`nuw`, so they wrap like `wrapping_add`,
//! - `/` and `%` test their divisor and call the runtime trap on zero, and
//!   special-case `MIN / -1` (which is poison in LLVM but a defined wrapping
//!   result in the VM),
//! - a local read *clones* its string and every consuming operation frees one,
//!   mirroring the VM's affine string heap — so a native run reclaims exactly
//!   what an interpreted run does.
//!
//! This module owns the per-body scaffold — the local slots and the state every
//! part of a body shares. The lowering itself splits by what it lowers:
//! [`stmt`] for statements and control flow, [`expr`] for expressions and
//! operators, and [`call`] for calls, including the crossing into the VM half.

mod boxes;
mod call;
mod cells;
mod compiler;
mod env;
mod expr;
mod file_system;
mod foreign;
mod foreign_aggregate;
mod foreign_field;
mod math;
mod operators;
mod stmt;
mod syscall;

use kira_ir::{IrExprId, IrFunction};
use kira_semantics_model::Type;
use llvm_sys::core::*;
use llvm_sys::prelude::*;
use llvm_sys::{LLVMLinkage, LLVMUnnamedAddr};

use super::ffi::c_string;
use super::{Callable, Codegen};
use crate::LlvmError;

impl<'a> Codegen<'a> {
    /// Lowers one Kira function body.
    pub(super) fn lower_function(
        &mut self,
        index: usize,
        function: &'a IrFunction,
    ) -> Result<(), LlvmError> {
        let value = self.functions[index]
            .ok_or(LlvmError::internal(
                "a body for a function on the other engine",
            ))?
            .value;

        // SAFETY: `value` is a function in this live module; the builder is
        // positioned on its entry block before any instruction is built.
        let entry = unsafe {
            let entry = LLVMAppendBasicBlockInContext(self.context, value, c"entry".as_ptr());
            LLVMPositionBuilderAtEnd(self.builder, entry);
            entry
        };
        self.begin_debug_function(index, value);

        let locals = self.allocate_locals(function, value)?;
        if let Some(debug) = self.debug.as_ref() {
            debug.declare_locals(
                index,
                function.param_count as usize,
                &function.locals,
                &locals,
                entry,
            );
        }
        let drop_glue = self
            .program
            .types
            .structs()
            .defs()
            .iter()
            .any(|def| def.drop_glue == Some(index as u32));
        let live = self.allocate_live_flags(function, &locals)?;
        let mut body = FunctionLowering {
            codegen: self,
            function,
            drop_glue,
            locals,
            live,
            loops: Vec::new(),
        };
        body.lower_block(&function.body)?;
        body.finish()
    }

    /// Allocates one stack slot per local and initializes every one.
    ///
    /// Slots start at their type's zero (`0`, `0.0`, `false`, the null string
    /// handle), mirroring the VM initializing every slot to `Void`: a `let` in
    /// a loop body then frees the previous iteration's string through the same
    /// path an assignment does, with no special case for the first store.
    fn allocate_locals(
        &mut self,
        function: &IrFunction,
        value: LLVMValueRef,
    ) -> Result<Vec<LLVMValueRef>, LlvmError> {
        let mut locals = Vec::with_capacity(function.locals.len());
        for (slot, &ty) in function.locals.iter().enumerate() {
            // A written-through parameter is a pointer into the caller's
            // storage, passed in its own position. The slot *is* that pointer —
            // every read and write of the parameter goes through it, mutating
            // the caller in place — so it is neither allocated nor initialized
            // here, and (see `emit_return`) never freed here either.
            if self.param_is_pointer(function, slot as u32) {
                // SAFETY: `value` is this function; this parameter is the
                // pointer `declare_function` gave it.
                locals.push(unsafe { LLVMGetParam(value, slot as u32) });
                continue;
            }
            let llvm_type = if function
                .native_state_locals
                .get(slot)
                .copied()
                .flatten()
                .is_some()
            {
                self.types.i64
            } else {
                self.llvm_type(ty)?
            };
            let name = c_string(&format!("local.{slot}"));
            // SAFETY: the builder sits on the function's entry block, and every
            // type and value below comes from this module's context.
            let alloca = unsafe { LLVMBuildAlloca(self.builder, llvm_type, name.as_ptr()) };
            let native_state = function
                .native_state_locals
                .get(slot)
                .copied()
                .flatten()
                .is_some();
            if (slot as u32) < function.param_count {
                // Parameters take ownership of the caller's argument, just as
                // the VM moves arguments into the callee's slots.
                // SAFETY: `value` is this function and the parameter has the
                // slot's type.
                unsafe { LLVMBuildStore(self.builder, LLVMGetParam(value, slot as u32), alloca) };
            } else if native_state {
                // SAFETY: `i64` belongs to this module's context.
                unsafe { LLVMBuildStore(self.builder, LLVMConstInt(self.types.i64, 0, 0), alloca) };
            } else {
                let zero = self.zero_value(ty)?;
                self.store_zero(alloca, zero, llvm_type);
            }
            locals.push(alloca);
        }
        Ok(locals)
    }

    /// Allocates the liveness flag of every heap-owning local and initializes
    /// it. Scope releases can happen before a frame returns, so the final
    /// release plan needs the same empty/non-empty bit for strings, arrays,
    /// enums, and ordinary aggregates as it already needs for user `Drop`.
    ///
    /// A parameter arrives holding a value, so its flag starts set; every other
    /// slot starts holding its type's zero, which is nothing.
    fn allocate_live_flags(
        &mut self,
        function: &IrFunction,
        locals: &[LLVMValueRef],
    ) -> Result<Vec<Option<LLVMValueRef>>, LlvmError> {
        let mut flags = Vec::with_capacity(locals.len());
        for (slot, &ty) in function.locals.iter().enumerate() {
            if !self.program.types.owns_heap(ty) {
                flags.push(None);
                continue;
            }
            let name = c_string(&format!("local.{slot}.live"));
            // SAFETY: the builder sits on the function's entry block and `i1`
            // belongs to this module's context.
            let flag = unsafe { LLVMBuildAlloca(self.builder, self.types.i1, name.as_ptr()) };
            let initial = u64::from((slot as u32) < function.param_count);
            // SAFETY: same block, and `i1` matches the slot just allocated.
            unsafe {
                LLVMBuildStore(self.builder, LLVMConstInt(self.types.i1, initial, 0), flag);
            }
            flags.push(Some(flag));
        }
        Ok(flags)
    }

    /// An `Int` constant.
    ///
    /// Constants are module-level values, so unlike instructions they need no
    /// builder position — only types from this context.
    pub(super) fn const_int(&self, value: i64) -> LLVMValueRef {
        // SAFETY: `i64` belongs to this module's live context.
        unsafe { LLVMConstInt(self.types.i64, value as u64, 1) }
    }

    /// The handle for a payload-less enum variant.
    ///
    /// The whole value is its tag, so the handle carries it — `(tag << 1) | 1`,
    /// a constant with no allocation behind it. The runtime reads the low bit
    /// and treats such a handle as owning nothing; see
    /// `kira_native_bridge::enums::is_inline` for the other side of the
    /// contract.
    pub(super) fn inline_enum(&self, tag: u32) -> LLVMValueRef {
        let word = (u64::from(tag) << 1) | 1;
        // SAFETY: both types belong to this module's live context, and a
        // constant needs no builder position.
        unsafe { LLVMConstIntToPtr(LLVMConstInt(self.types.i64, word, 0), self.types.ptr) }
    }

    /// The same handle, for a tag that is only known at run time.
    ///
    /// The seam builds one of these: a variant tag arrives as a value, not as a
    /// constant, so [`Codegen::inline_enum`] cannot make the handle. Kept
    /// beside it, and deliberately not open-coded at the call site — the
    /// encoding is a contract with `kira_native_bridge::enums::is_inline`, and a
    /// second copy of it is a second thing to keep in step.
    pub(in crate::codegen) fn inline_enum_value(&self, tag: LLVMValueRef) -> LLVMValueRef {
        // SAFETY: `tag` is an `i64` and the builder is on a live block; both
        // types belong to this module's live context.
        unsafe {
            let one = LLVMConstInt(self.types.i64, 1, 0);
            let shifted = LLVMBuildShl(self.builder, tag, one, c"enum.inline.shl".as_ptr());
            let word = LLVMBuildOr(self.builder, shifted, one, c"enum.inline.word".as_ptr());
            LLVMBuildIntToPtr(self.builder, word, self.types.ptr, c"enum.inline".as_ptr())
        }
    }

    /// A `usize` constant at the **target**'s pointer width.
    ///
    /// The runtime helpers that take a length take a `usize`, and on wasm32
    /// that is 32 bits. A 64-bit constant there is a signature mismatch the
    /// linker resolves by name and the module traps on.
    pub(super) fn const_usize(&self, value: u64) -> LLVMValueRef {
        // SAFETY: `usize_ty` belongs to this module's live context.
        unsafe { LLVMConstInt(self.types.usize_ty, value, 0) }
    }

    /// A `Float` constant.
    pub(super) fn const_float(&self, value: f64) -> LLVMValueRef {
        // SAFETY: `f64` belongs to this module's live context.
        unsafe { LLVMConstReal(self.types.f64, value) }
    }

    /// A `Bool` constant.
    pub(super) fn const_bool(&self, value: bool) -> LLVMValueRef {
        // SAFETY: `i1` belongs to this module's live context.
        unsafe { LLVMConstInt(self.types.i1, u64::from(value), 0) }
    }

    /// The zero value a fresh local slot holds.
    fn zero_value(&self, ty: Type) -> Result<LLVMValueRef, LlvmError> {
        let llvm_type = self.llvm_type(ty)?;
        // SAFETY: `llvm_type` belongs to this module's context.
        Ok(unsafe {
            match ty {
                Type::Int(_) | Type::Bool => LLVMConstInt(llvm_type, 0, 0),
                Type::Float(_) => LLVMConstReal(llvm_type, 0.0),
                // A fresh array slot holds the null handle, which the runtime's
                // `array_len` reads as `0` and `array_free` treats as nothing to
                // free — so a slot is reclaimable through the same path before
                // its first store, exactly as a `String`'s null handle is. An
                // enum handle is the same: `kira_rt_enum_free` treats null as
                // nothing to free.
                // An `Any` slot is the same null handle for the same reason: its
                // box *is* an enum box, so `kira_rt_enum_free` reads null as
                // nothing to free and a slot is reclaimable before its first
                // store.
                // A fresh cell slot holds the null handle too:
                // `kira_rt_cell_free` reads null as nothing to free, so a slot
                // is reclaimable before the `CellNew` that fills it — which is
                // what a slot inside a branch that never ran needs.
                Type::String | Type::Array(_) | Type::Enum(_) | Type::Any | Type::Cell(_) => {
                    LLVMConstPointerNull(llvm_type)
                }
                // A fresh `RawPtr` slot holds the null pointer word (zero), the
                // same value the VM initializes a `Value::RawPtr` slot to. It
                // owns nothing, so no first-store special case is needed.
                Type::RawPtr
                | Type::ForeignPtr(_)
                | Type::NativeState(_)
                | Type::Task(_)
                | Type::CBlock => LLVMConstInt(llvm_type, 0, 0),
                // `CString` is seam-only and never names a local slot.
                Type::CString => {
                    return Err(LlvmError::internal(
                        "a CString local (it is a foreign-parameter-only type)",
                    ));
                }
                // Every field zeroed, which for a `String` field is the null
                // handle the runtime already reads as `""` — so a fresh struct
                // slot is free-able through the same path as any other, with no
                // first-store special case.
                Type::Struct(_) => LLVMConstNull(llvm_type),
                Type::Void | Type::Error => {
                    return Err(LlvmError::internal("a local with no runtime value"));
                }
            }
        })
    }

    /// Builds a private constant global holding `text`, returning a pointer to
    /// its bytes (the null pointer for the empty string, which never
    /// allocates).
    fn string_constant(&mut self, text: &str) -> LLVMValueRef {
        let bytes = text.as_bytes();
        // SAFETY: every type and value below is from this live module; `bytes`
        // outlives the constant-array copy LLVM makes.
        unsafe {
            if bytes.is_empty() {
                return LLVMConstPointerNull(self.types.ptr);
            }
            let name = c_string(&format!("kira.str.{}", self.string_counter));
            self.string_counter += 1;
            let initializer = LLVMConstStringInContext2(
                self.context,
                bytes.as_ptr().cast(),
                bytes.len(),
                1, // Kira strings carry their length; no NUL terminator.
            );
            let array = LLVMArrayType2(self.types.i8, bytes.len() as u64);
            let global = LLVMAddGlobal(self.module, array, name.as_ptr());
            LLVMSetInitializer(global, initializer);
            LLVMSetGlobalConstant(global, 1);
            LLVMSetLinkage(global, LLVMLinkage::LLVMPrivateLinkage);
            // Identical literals may share storage: the runtime copies out of
            // them and never compares their addresses.
            LLVMSetUnnamedAddress(global, LLVMUnnamedAddr::LLVMGlobalUnnamedAddr);
            global
        }
    }

    /// The same, NUL-terminated, for a runtime helper that reads a C string.
    ///
    /// [`Codegen::string_constant`] deliberately writes no terminator — a Kira
    /// string carries its length — so handing one to a helper that calls
    /// `CStr::from_ptr` reads past its end into whatever constant the linker
    /// laid down next. That is not hypothetical: the missing-library trap
    /// printed `kira_metal` glued to the mesh name that happened to follow it.
    fn c_string_constant(&mut self, text: &str) -> LLVMValueRef {
        let bytes = text.as_bytes();
        // SAFETY: every type and value below is from this live module; `bytes`
        // outlives the constant-array copy LLVM makes.
        unsafe {
            let name = c_string(&format!("kira.cstr.{}", self.string_counter));
            self.string_counter += 1;
            let initializer = LLVMConstStringInContext2(
                self.context,
                bytes.as_ptr().cast(),
                bytes.len(),
                0, // NUL-terminated: a C string is read to its terminator.
            );
            let array = LLVMArrayType2(self.types.i8, bytes.len() as u64 + 1);
            let global = LLVMAddGlobal(self.module, array, name.as_ptr());
            LLVMSetInitializer(global, initializer);
            LLVMSetGlobalConstant(global, 1);
            LLVMSetLinkage(global, LLVMLinkage::LLVMPrivateLinkage);
            LLVMSetUnnamedAddress(global, LLVMUnnamedAddr::LLVMGlobalUnnamedAddr);
            global
        }
    }
}

/// Lowering state for one function body.
pub(super) struct FunctionLowering<'a, 'p> {
    pub(super) codegen: &'a mut Codegen<'p>,
    pub(super) function: &'p IrFunction,
    /// Whether this function is a type's user `Drop` body, which is what
    /// excludes its receiver from the release plan.
    pub(super) drop_glue: bool,
    /// One `i1` flag per local whose type runs a user `Drop`, `None` for every
    /// other slot.
    ///
    /// A slot starts at its type's zero, which for every other type is a value
    /// a release can be handed — a null handle frees nothing. A user `Drop`
    /// body has no such reading of zero: it would run on a value nobody wrote.
    /// So the slot carries whether anything has been stored in it, and the
    /// release asks. The VM needs none of this: its slots start at `Void`,
    /// which is not a struct, so the same release does nothing there.
    live: Vec<Option<LLVMValueRef>>,
    /// One `alloca` per local slot, in slot order.
    locals: Vec<LLVMValueRef>,
    /// The loops enclosing the statement being lowered, innermost last.
    ///
    /// A `break`/`continue` branches to a block of the innermost, so it reads
    /// the top of this stack.
    loops: Vec<LoopBlocks>,
}

/// The blocks a `break`/`continue` inside one loop branches to.
struct LoopBlocks {
    /// The condition test — the target of a `continue`, which re-tests before
    /// iterating, exactly as falling off the body's end does.
    test: LLVMBasicBlockRef,
    /// The block after the loop — the target of a `break`.
    exit: LLVMBasicBlockRef,
}

impl FunctionLowering<'_, '_> {
    /// Terminates the body when control can still fall off its end.
    ///
    /// A `Void` function returns unit, mirroring the bytecode compiler's
    /// trailing `ReturnVoid`. For a value-returning function the analyzer has
    /// already proved every path returns, so the fall-through is unreachable —
    /// and saying so lets LLVM keep the guarantee rather than inventing a value.
    fn finish(&mut self) -> Result<(), LlvmError> {
        if self.block_is_terminated() {
            return Ok(());
        }
        if self.function.return_type == Type::Void {
            self.emit_return(None)
        } else {
            // SAFETY: the builder is positioned on an unterminated block.
            unsafe { LLVMBuildUnreachable(self.codegen.builder) };
            Ok(())
        }
    }

    /// Whether a value of `ty` owns heap storage that a copy must clone and a
    /// drop must release.
    fn owns_heap(&self, ty: Type) -> bool {
        self.codegen.owns_heap(ty)
    }

    /// Produces an independent copy of `value`, mirroring the VM's
    /// `Heap::copy_value`. See [`Codegen::copy_value`].
    pub(super) fn copy_value(
        &mut self,
        value: LLVMValueRef,
        ty: Type,
    ) -> Result<LLVMValueRef, LlvmError> {
        self.codegen.copy_value(value, ty)
    }

    /// Releases whatever heap storage `value` owns, mirroring the VM's
    /// `Heap::drop_value`. See [`Codegen::drop_value`].
    pub(super) fn drop_value(&mut self, value: LLVMValueRef, ty: Type) -> Result<(), LlvmError> {
        self.codegen.drop_value(value, ty)
    }

    /// Reads field `index` out of a struct *value*.
    fn extract_field(
        &mut self,
        value: LLVMValueRef,
        index: u32,
    ) -> Result<LLVMValueRef, LlvmError> {
        Ok(self.codegen.extract_field(value, index))
    }

    /// Returns `value` with field `index` replaced by `field`.
    fn insert_field(
        &mut self,
        value: LLVMValueRef,
        field: LLVMValueRef,
        index: u32,
    ) -> Result<LLVMValueRef, LlvmError> {
        Ok(self.codegen.insert_field(value, field, index))
    }

    /// Emits a call to `callable`.
    pub(in crate::codegen) fn call(
        &mut self,
        callable: Callable,
        args: &mut [LLVMValueRef],
        name: &std::ffi::CStr,
    ) -> LLVMValueRef {
        // SAFETY: the builder is on a live block and `args` matches the
        // callable's signature at every call site above.
        unsafe { self.codegen.call_runtime(callable, args, name) }
    }

    /// The static type of an expression in this function's scope.
    pub(super) fn type_of(&self, id: IrExprId) -> Type {
        self.codegen.program.expr_type(self.function, id)
    }

    /// The declared type of a local slot.
    pub(super) fn local_type(&self, slot: u32) -> Result<Type, LlvmError> {
        self.function
            .locals
            .get(slot as usize)
            .copied()
            .ok_or(LlvmError::internal("a read of an unknown local"))
    }

    /// The liveness flag of a local whose type runs a user `Drop`.
    pub(super) fn live_flag(&self, slot: u32) -> Option<LLVMValueRef> {
        self.live.get(slot as usize).copied().flatten()
    }

    /// The `alloca` backing a local slot.
    pub(super) fn local_pointer(&self, slot: u32) -> Result<LLVMValueRef, LlvmError> {
        self.locals
            .get(slot as usize)
            .copied()
            .ok_or(LlvmError::internal("a read of an unknown local"))
    }

    /// The function currently being built.
    fn current_function(&self) -> LLVMValueRef {
        // SAFETY: the builder is always positioned inside a function while a
        // body is being lowered.
        unsafe { LLVMGetBasicBlockParent(LLVMGetInsertBlock(self.codegen.builder)) }
    }

    /// Appends a fresh block to `function`.
    fn append_block(&self, function: LLVMValueRef, name: &std::ffi::CStr) -> LLVMBasicBlockRef {
        // SAFETY: `function` is a live function in this module's context.
        unsafe { LLVMAppendBasicBlockInContext(self.codegen.context, function, name.as_ptr()) }
    }

    /// Moves the builder to the end of `block`.
    fn position_at(&self, block: LLVMBasicBlockRef) {
        // SAFETY: `block` belongs to the function being built.
        unsafe { LLVMPositionBuilderAtEnd(self.codegen.builder, block) };
    }

    /// Whether the block being built already ends in a terminator.
    fn block_is_terminated(&self) -> bool {
        // SAFETY: the builder is positioned on a live block whenever this is
        // asked.
        unsafe { !LLVMGetBasicBlockTerminator(LLVMGetInsertBlock(self.codegen.builder)).is_null() }
    }
}
