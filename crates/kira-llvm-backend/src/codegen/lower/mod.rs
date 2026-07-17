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

mod call;
mod expr;
mod stmt;

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
            .ok_or(LlvmError::Unsupported(
                "a body for a function on the other engine",
            ))?
            .value;

        // SAFETY: `value` is a function in this live module; the builder is
        // positioned on its entry block before any instruction is built.
        unsafe {
            let entry = LLVMAppendBasicBlockInContext(self.context, value, c"entry".as_ptr());
            LLVMPositionBuilderAtEnd(self.builder, entry);
        }

        let locals = self.allocate_locals(function, value)?;
        let mut body = FunctionLowering {
            codegen: self,
            function,
            locals,
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
            let llvm_type = self.llvm_type(ty)?;
            let name = c_string(&format!("local.{slot}"));
            // SAFETY: the builder sits on the function's entry block, and every
            // type and value below comes from this module's context.
            unsafe {
                let alloca = LLVMBuildAlloca(self.builder, llvm_type, name.as_ptr());
                let initial = if (slot as u32) < function.param_count {
                    // Parameters take ownership of the caller's argument, just
                    // as the VM moves arguments into the callee's slots.
                    LLVMGetParam(value, slot as u32)
                } else {
                    self.zero_value(ty)?
                };
                LLVMBuildStore(self.builder, initial, alloca);
                locals.push(alloca);
            }
        }
        Ok(locals)
    }

    /// An `Int` constant.
    ///
    /// Constants are module-level values, so unlike instructions they need no
    /// builder position — only types from this context.
    pub(super) fn const_int(&self, value: i64) -> LLVMValueRef {
        // SAFETY: `i64` belongs to this module's live context.
        unsafe { LLVMConstInt(self.types.i64, value as u64, 1) }
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
                Type::Int | Type::Bool => LLVMConstInt(llvm_type, 0, 0),
                Type::Float => LLVMConstReal(llvm_type, 0.0),
                Type::String => LLVMConstPointerNull(llvm_type),
                // Every field zeroed, which for a `String` field is the null
                // handle the runtime already reads as `""` — so a fresh struct
                // slot is free-able through the same path as any other, with no
                // first-store special case.
                Type::Struct(_) => LLVMConstNull(llvm_type),
                Type::Void | Type::Error => {
                    return Err(LlvmError::Unsupported("a local with no runtime value"));
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
}

/// Lowering state for one function body.
struct FunctionLowering<'a, 'p> {
    codegen: &'a mut Codegen<'p>,
    function: &'p IrFunction,
    /// One `alloca` per local slot, in slot order.
    locals: Vec<LLVMValueRef>,
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

    /// Frees a string handle through the runtime.
    fn free_string(&mut self, value: LLVMValueRef) {
        self.call(self.codegen.runtime.str_free, &mut [value], c"");
    }

    /// Whether a value of `ty` owns heap storage that a copy must clone and a
    /// drop must release.
    fn owns_heap(&self, ty: Type) -> bool {
        self.codegen.program.structs.owns_heap(ty)
    }

    /// Produces an independent copy of `value`, mirroring the VM's
    /// `Heap::copy_value`.
    ///
    /// Deep, field by field: a struct's copy clones every string it reaches, so
    /// no two live values share a handle and neither drop frees the other's.
    /// Scalars and structs of scalars copy for free — LLVM's `insertvalue`
    /// chain folds away — so the walk only costs anything where it must.
    fn copy_value(&mut self, value: LLVMValueRef, ty: Type) -> Result<LLVMValueRef, LlvmError> {
        if !self.owns_heap(ty) {
            return Ok(value);
        }
        match ty {
            Type::String => {
                Ok(self.call(self.codegen.runtime.str_clone, &mut [value], c"str.copy"))
            }
            Type::Struct(id) => {
                let field_types = self.field_types(id)?;
                let mut copy = value;
                for (index, field_ty) in field_types.into_iter().enumerate() {
                    if !self.owns_heap(field_ty) {
                        continue;
                    }
                    let field = self.extract_field(value, index as u32)?;
                    let copied = self.copy_value(field, field_ty)?;
                    copy = self.insert_field(copy, copied, index as u32)?;
                }
                Ok(copy)
            }
            // `owns_heap` is only true for the two cases above.
            _ => Err(LlvmError::Unsupported("a copy of an unowned value")),
        }
    }

    /// Releases whatever heap storage `value` owns, mirroring the VM's
    /// `Heap::drop_value`.
    fn drop_value(&mut self, value: LLVMValueRef, ty: Type) -> Result<(), LlvmError> {
        if !self.owns_heap(ty) {
            return Ok(());
        }
        match ty {
            Type::String => {
                self.free_string(value);
                Ok(())
            }
            Type::Struct(id) => {
                let field_types = self.field_types(id)?;
                for (index, field_ty) in field_types.into_iter().enumerate() {
                    if !self.owns_heap(field_ty) {
                        continue;
                    }
                    let field = self.extract_field(value, index as u32)?;
                    self.drop_value(field, field_ty)?;
                }
                Ok(())
            }
            _ => Err(LlvmError::Unsupported("a drop of an unowned value")),
        }
    }

    /// The field types of a declared struct.
    fn field_types(&self, id: kira_semantics_model::StructId) -> Result<Vec<Type>, LlvmError> {
        self.codegen
            .program
            .structs
            .get(id)
            .map(|def| def.fields.iter().map(|field| field.ty).collect())
            .ok_or(LlvmError::Unsupported(
                "a struct the program never declared",
            ))
    }

    /// Reads field `index` out of a struct *value*.
    fn extract_field(
        &mut self,
        value: LLVMValueRef,
        index: u32,
    ) -> Result<LLVMValueRef, LlvmError> {
        let name = c_string(&format!("field.{index}"));
        // SAFETY: `value` is a struct value with more than `index` fields — the
        // index came from that struct's own definition — and the builder is on
        // a live block.
        Ok(unsafe { LLVMBuildExtractValue(self.codegen.builder, value, index, name.as_ptr()) })
    }

    /// Returns `value` with field `index` replaced by `field`.
    fn insert_field(
        &mut self,
        value: LLVMValueRef,
        field: LLVMValueRef,
        index: u32,
    ) -> Result<LLVMValueRef, LlvmError> {
        let name = c_string(&format!("with.{index}"));
        // SAFETY: as `extract_field`, and `field` has field `index`'s type.
        Ok(unsafe {
            LLVMBuildInsertValue(self.codegen.builder, value, field, index, name.as_ptr())
        })
    }

    /// Emits a call to `callable`.
    fn call(
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
    fn type_of(&self, id: IrExprId) -> Type {
        self.codegen.program.expr_type(self.function, id)
    }

    /// The declared type of a local slot.
    fn local_type(&self, slot: u32) -> Result<Type, LlvmError> {
        self.function
            .locals
            .get(slot as usize)
            .copied()
            .ok_or(LlvmError::Unsupported("a read of an unknown local"))
    }

    /// The `alloca` backing a local slot.
    fn local_pointer(&self, slot: u32) -> Result<LLVMValueRef, LlvmError> {
        self.locals
            .get(slot as usize)
            .copied()
            .ok_or(LlvmError::Unsupported("a read of an unknown local"))
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
