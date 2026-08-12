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
//! entirely. Only `[String]`, `[SomeStruct]`, and `[[Element]]` cost a leaf — which
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
use super::types::Callable;

/// Which leaf is being emitted, for the two that share a shape.
///
/// Visible to the parent module because [`Codegen`]'s `element_leaves` cache is
/// keyed by it — one entry per `(element type, leaf)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum Leaf {
    /// `(src, dst)`: make `*dst`, which already holds a bitwise copy of `*src`,
    /// independently owned.
    ///
    /// Every caller in `kira-native-bridge` copies the bytes across first — see
    /// `array::make_unique` and `enums::clone_aggregate` — and a Kira copy
    /// raises share counts without changing bits, so what remains for this leaf
    /// is to take a share of everything the destination now names. `src` is
    /// therefore read by nobody, and stays in the signature because it is the
    /// runtime's `ElemClone`.
    Clone,
    /// `(at)`: take a share of everything `*at` owns.
    ///
    /// The compiler's own half of [`Leaf::Clone`]: generated code has an
    /// address for the value it is copying and no use for the second parameter.
    Retain,
    /// `(at)`: release whatever `*at` owns.
    Free,
    /// `(a, b) -> i8`: whether `*a` and `*b` are structurally equal.
    ///
    /// Unlike the other two, this one has no "owns nothing" shortcut. A flat
    /// `memcmp` would read padding bytes, which a copy never defines, so even a
    /// struct of two `Int`s needs a real leaf that compares the fields it has.
    Eq,
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

    /// The ABI alignment of `ty`, as an `i64` constant.
    ///
    /// LLVM's answer for the target, for the same reason [`Self::abi_size`]
    /// takes its answer from there: a box the runtime allocates for a value has
    /// to satisfy the alignment the target gives that value, not one guessed at
    /// here.
    pub(super) fn abi_align(&self, ty: Type) -> Result<LLVMValueRef, LlvmError> {
        let llvm_type = self.llvm_type(ty)?;
        // SAFETY: `llvm_type` belongs to this module's context, and the data
        // layout was set when the module was created.
        let align =
            unsafe { llvm_sys::target::LLVMABIAlignmentOfType(self.target_data, llvm_type) };
        // SAFETY: `types.i64` is this context's `i64`.
        Ok(unsafe { LLVMConstInt(self.types.i64, u64::from(align), 0) })
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

    /// The equality leaf for a value of `ty`, for comparing it once erased.
    ///
    /// Always a real function: see [`Leaf::Eq`] for why there is no null case.
    pub(in crate::codegen) fn element_eq(&mut self, ty: Type) -> Result<LLVMValueRef, LlvmError> {
        self.element_leaf(ty, Leaf::Eq)
    }

    /// The leaf for `ty`, paired with its signature so it can be *called*.
    ///
    /// [`Codegen::element_clone`] and [`Codegen::element_free`] hand a leaf to
    /// the runtime as a function pointer, where a null one means "nothing to
    /// do". A caller that means to call the leaf itself needs a real function,
    /// so this is only for a type that owns heap storage — the one case where
    /// the two clone and free leaves are never null.
    pub(super) fn leaf_callable(&mut self, ty: Type, leaf: Leaf) -> Result<Callable, LlvmError> {
        debug_assert!(
            leaf == Leaf::Eq || self.program.types.owns_heap(ty),
            "a callable clone or free leaf for a type that owns nothing"
        );
        let value = self.element_leaf(ty, leaf)?;
        Ok(Callable {
            ty: self.leaf_signature(leaf),
            value,
        })
    }

    /// The LLVM signature every leaf of one kind shares.
    ///
    /// Through memory, because that is what the runtime's array helpers can
    /// call: they hold element slots, not values. It is also what keeps a
    /// struct's walk off a load of the whole struct — see [`super::glue`].
    fn leaf_signature(&self, leaf: Leaf) -> LLVMTypeRef {
        let mut params = match leaf {
            Leaf::Clone | Leaf::Eq => vec![self.types.ptr, self.types.ptr],
            Leaf::Retain | Leaf::Free => vec![self.types.ptr],
        };
        let returns = match leaf {
            Leaf::Eq => self.types.i8,
            Leaf::Clone | Leaf::Retain | Leaf::Free => self.types.void,
        };
        // SAFETY: every type belongs to this module's context and `params`
        // outlives the call, which copies it.
        unsafe { LLVMFunctionType(returns, params.as_mut_ptr(), params.len() as u32, 0) }
    }

    fn element_leaf(&mut self, ty: Type, leaf: Leaf) -> Result<LLVMValueRef, LlvmError> {
        // An element that owns nothing needs no clone or free leaf, and saying
        // so with a null pointer is what lets the runtime skip its loop for
        // `[Int]`. Equality has no such case — it compares the value itself.
        if leaf != Leaf::Eq && !self.program.types.owns_heap(ty) {
            // SAFETY: `types.ptr` is this context's opaque pointer type.
            return Ok(unsafe { LLVMConstNull(self.types.ptr) });
        }
        if let Some(cached) = self.element_leaves.get(&(ty, leaf)) {
            return Ok(*cached);
        }
        // Declared and cached **before** its body is emitted. A type may reach
        // itself — a closure that captures a function value of its own type
        // holds it behind a one-element array, so the representation struct
        // contains an array of itself — and that leaf's body asks for the leaf
        // of the element type, which is this one. Caching the declaration first
        // is what turns that from unbounded recursion into an ordinary
        // recursive function.
        let (function, entry) = self.declare_element_leaf(ty, leaf);
        self.element_leaves.insert((ty, leaf), function);

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

    /// Declares one leaf and gives back its empty entry block.
    fn declare_element_leaf(&mut self, ty: Type, leaf: Leaf) -> (LLVMValueRef, LLVMBasicBlockRef) {
        let _ = ty;
        let ordinal = self.element_leaves.len() as u32;
        let name = c_string(&match leaf {
            Leaf::Clone => format!("kira.elem.clone.{ordinal}"),
            Leaf::Retain => format!("kira.elem.retain.{ordinal}"),
            Leaf::Free => format!("kira.elem.free.{ordinal}"),
            Leaf::Eq => format!("kira.elem.eq.{ordinal}"),
        });
        let signature = self.leaf_signature(leaf);

        // SAFETY: the signature belongs to this module's context, and the block
        // is appended to the function just created.
        let (function, entry) = unsafe {
            let function = LLVMAddFunction(self.module, name.as_ptr(), signature);
            // Internal: a leaf is this module's own, never part of its ABI.
            LLVMSetLinkage(function, llvm_sys::LLVMLinkage::LLVMInternalLinkage);
            let entry = LLVMAppendBasicBlockInContext(self.context, function, c"entry".as_ptr());
            (function, entry)
        };
        self.mark_always_inline(function);

        (function, entry)
    }

    /// The body of one leaf: the walk for `ty`, entered through the parameters.
    fn emit_leaf_body(
        &mut self,
        ty: Type,
        leaf: Leaf,
        function: LLVMValueRef,
    ) -> Result<(), LlvmError> {
        // SAFETY: the parameters exist — the signature was just built with
        // them — and the builder is positioned on the entry block.
        let src = unsafe { LLVMGetParam(function, 0) };

        match leaf {
            // The destination already holds the bytes (see [`Leaf::Clone`]), so
            // the copy is finished by taking a share of what they name.
            Leaf::Clone => {
                // SAFETY: `dst` is the second parameter of a two-parameter
                // signature, addressing an element slot of this type.
                let dst = unsafe { LLVMGetParam(function, 1) };
                self.retain_at_walk(dst, ty)?;
            }
            Leaf::Retain => self.retain_at_walk(src, ty)?,
            Leaf::Free => self.release_at_walk(src, ty)?,
            Leaf::Eq => {
                // SAFETY: `other` is the second parameter of a two-parameter
                // signature, addressing a live value of this type.
                let other = unsafe { LLVMGetParam(function, 1) };
                let equal = self.equal_at_walk(src, other, ty)?;
                // SAFETY: `equal` is an `i1`; the seam speaks `i8`, and the
                // builder is on an unterminated block of this function.
                unsafe {
                    let widened =
                        LLVMBuildZExt(self.builder, equal, self.types.i8, c"elem.eq".as_ptr());
                    LLVMBuildRet(self.builder, widened);
                }
                return Ok(());
            }
        }
        // SAFETY: the builder is on an unterminated block of this function.
        unsafe { LLVMBuildRetVoid(self.builder) };
        Ok(())
    }
}
