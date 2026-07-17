//! The per-element-type clone and free leaves an array's runtime helpers call.
//!
//! `kira_rt_array_clone` and `kira_rt_array_free` keep the *loop* — walking the
//! item block is ordinary Rust in `kira-native-bridge`, where it needs no basic
//! blocks and no phi nodes. What they cannot know is how to clone or free one
//! element, because that depends on the Kira type. So the backend emits a leaf
//! per element type and hands it over as a function pointer.
//!
//! ```text
//!   kira.elem.clone.<n>(src: ptr, dst: ptr)   // *dst = copy(*src)
//!   kira.elem.free.<n>(at: ptr)               // drop(*at)
//! ```
//!
//! # Why a null pointer is the common case
//!
//! An element that owns nothing needs neither leaf: the flat `memcpy` a clone
//! starts with is already a correct copy of an `Int`, and freeing one is
//! nothing. So `[Int]` passes null for both, and the runtime skips its loop
//! entirely. Only `[String]`, `[SomeStruct]`, and `[[T]]` cost a leaf — which
//! is the same line `owns_heap` already draws everywhere else.
//!
//! # Memoized per type
//!
//! Two arrays of the same element share one leaf. The cache is keyed by the
//! element type rather than the array type, so `[Int]` and a `[[Int]]`'s inner
//! `[Int]` do not emit it twice.

use kira_semantics_model::Type;
use llvm_sys::core::*;
use llvm_sys::prelude::*;
use llvm_sys::target::LLVMABISizeOfType;

use crate::LlvmError;

use super::Codegen;
use super::c_string;

/// Which leaf is being emitted, for the two that share a shape.
///
/// Visible to the parent module because [`Codegen`]'s `element_leaves` cache is
/// keyed by it — one entry per `(element type, leaf)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum Leaf {
    /// `(src, dst)`: write an independent copy of `*src` to `*dst`.
    Clone,
    /// `(at)`: release whatever `*at` owns.
    Free,
}

impl Codegen<'_> {
    /// The ABI size of one element of `ty`, as an `i64` constant.
    ///
    /// LLVM's own answer for the target, not one computed here: the stride an
    /// array uses has to be the one the target's ABI gives the element type, or
    /// a struct element would be read at the wrong offset.
    pub(super) fn abi_size(&self, ty: Type) -> Result<LLVMValueRef, LlvmError> {
        let llvm_type = self.llvm_type(ty)?;
        // SAFETY: `llvm_type` belongs to this module's context, and the data
        // layout was set when the module was created.
        let size = unsafe { LLVMABISizeOfType(self.target_data, llvm_type) };
        // SAFETY: `types.i64` is this context's `i64`.
        Ok(unsafe { LLVMConstInt(self.types.i64, size, 0) })
    }

    /// The clone leaf for an element of `ty`, or a null pointer when the flat
    /// copy already got it right.
    pub(super) fn element_clone(&mut self, ty: Type) -> Result<LLVMValueRef, LlvmError> {
        self.element_leaf(ty, Leaf::Clone)
    }

    /// The free leaf for an element of `ty`, or a null pointer when an element
    /// owns nothing to free.
    pub(super) fn element_free(&mut self, ty: Type) -> Result<LLVMValueRef, LlvmError> {
        self.element_leaf(ty, Leaf::Free)
    }

    fn element_leaf(&mut self, ty: Type, leaf: Leaf) -> Result<LLVMValueRef, LlvmError> {
        // An element that owns nothing needs no leaf, and saying so with a null
        // pointer is what lets the runtime skip its loop for `[Int]`.
        if !self.program.types.owns_heap(ty) {
            // SAFETY: `types.ptr` is this context's opaque pointer type.
            return Ok(unsafe { LLVMConstNull(self.types.ptr) });
        }
        if let Some(cached) = self.element_leaves.get(&(ty, leaf)) {
            return Ok(*cached);
        }
        let function = self.define_element_leaf(ty, leaf)?;
        self.element_leaves.insert((ty, leaf), function);
        Ok(function)
    }

    /// Emits one leaf's body.
    fn define_element_leaf(&mut self, ty: Type, leaf: Leaf) -> Result<LLVMValueRef, LlvmError> {
        let ordinal = self.element_leaves.len() as u32;
        let name = c_string(&match leaf {
            Leaf::Clone => format!("kira.elem.clone.{ordinal}"),
            Leaf::Free => format!("kira.elem.free.{ordinal}"),
        });
        let mut params = match leaf {
            Leaf::Clone => vec![self.types.ptr, self.types.ptr],
            Leaf::Free => vec![self.types.ptr],
        };

        // SAFETY: every type belongs to this module's context; `params`
        // outlives the `LLVMFunctionType` call; and the block is appended to
        // the function just created.
        let (function, entry) = unsafe {
            let signature =
                LLVMFunctionType(self.types.void, params.as_mut_ptr(), params.len() as u32, 0);
            let function = LLVMAddFunction(self.module, name.as_ptr(), signature);
            // Internal: a leaf is this module's own, never part of its ABI.
            LLVMSetLinkage(function, llvm_sys::LLVMLinkage::LLVMInternalLinkage);
            let entry = LLVMAppendBasicBlockInContext(self.context, function, c"entry".as_ptr());
            (function, entry)
        };

        // The leaf is emitted between whatever else is being built, so the
        // builder's position is saved and restored around it.
        // SAFETY: the builder is live; `entry` belongs to `function`.
        let resume = unsafe { LLVMGetInsertBlock(self.builder) };
        // SAFETY: `entry` is an empty block of a function in this module.
        unsafe { LLVMPositionBuilderAtEnd(self.builder, entry) };

        let emitted = self.emit_leaf_body(ty, leaf, function);

        // SAFETY: `resume` is the block the caller was building into, or null
        // when there was none (the leaf was requested outside a function body).
        unsafe {
            if !resume.is_null() {
                LLVMPositionBuilderAtEnd(self.builder, resume);
            }
        }
        emitted?;
        Ok(function)
    }

    /// The body of one leaf: load, act, and (for a clone) store back.
    fn emit_leaf_body(
        &mut self,
        ty: Type,
        leaf: Leaf,
        function: LLVMValueRef,
    ) -> Result<(), LlvmError> {
        let llvm_type = self.llvm_type(ty)?;
        // SAFETY: the parameters exist — the signature was just built with
        // them — and the builder is positioned on the entry block.
        let src = unsafe { LLVMGetParam(function, 0) };

        match leaf {
            Leaf::Clone => {
                // SAFETY: `dst` is the second parameter of a two-parameter
                // signature; both point at element slots of `llvm_type`.
                let dst = unsafe { LLVMGetParam(function, 1) };
                // SAFETY: `src` points at a live element of `llvm_type`.
                let value =
                    unsafe { LLVMBuildLoad2(self.builder, llvm_type, src, c"elem".as_ptr()) };
                let copied = self.copy_value(value, ty)?;
                // SAFETY: `dst` points at a slot of `llvm_type` and `copied`
                // has that type.
                unsafe { LLVMBuildStore(self.builder, copied, dst) };
            }
            Leaf::Free => {
                // SAFETY: `src` points at a live element of `llvm_type`.
                let value =
                    unsafe { LLVMBuildLoad2(self.builder, llvm_type, src, c"elem".as_ptr()) };
                self.drop_value(value, ty)?;
            }
        }
        // SAFETY: the builder is on an unterminated block of this function.
        unsafe { LLVMBuildRetVoid(self.builder) };
        Ok(())
    }
}
