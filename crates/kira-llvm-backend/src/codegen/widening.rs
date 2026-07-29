//! The generated rebuild that carries one generic instantiation into another
//! whose type arguments are `Any`.
//!
//! `Result<Int, E>` and `Result<Any, E>` are one value on the VM and two
//! different objects here. A `Result<Int, E>.Ok(10)` is a box holding `{tag 0,
//! INERT, 10}` — the ten is the payload, inline. A `Result<Any, E>.Ok(…)` is a
//! box holding `{tag 0, ENUM, ptr}`, and that pointer addresses a second box
//! holding the ten. So the assignment cannot be a no-op on this side, and
//! letting it be one is not a shortcut but a bug with a delay on it: a later
//! `match` would bind a payload statically typed `Any`, and copying that value
//! would run `kira_rt_enum_clone` on the number ten.
//!
//! ```text
//!   kira.widen.<n>(v: ptr) -> ptr   // consumes v, returns the rebuilt value
//! ```
//!
//! # Why a function per type pair
//!
//! The same reason [`super::elements`] emits a clone leaf per element type: the
//! shape is a program-wide fact, the call sites are many, and a template whose
//! payload is an instantiation of itself needs the recursion to be a *call*
//! rather than unbounded inlining. Declaring the function and caching it before
//! its body is emitted is what turns that self-reference into an ordinary
//! recursive function.
//!
//! # Why most variants cost nothing
//!
//! Only the variants whose payload type actually changed are rebuilt. Every
//! other tag — `Error(TestFailure)` in the case that matters, and every
//! payload-less variant — falls to the switch's default, which hands the value
//! straight back with its ownership intact. A pair with no changed variant at
//! all gets no function and no call.

use kira_semantics_model::{EnumId, Type};
use llvm_sys::core::*;
use llvm_sys::prelude::*;

use super::Codegen;
use super::ffi::c_string;
use super::types::Callable;
use crate::LlvmError;

/// One variant the rebuild has to do something about.
struct ChangedVariant {
    /// The variant's tag, which the switch compares against.
    tag: u32,
    /// The payload type the source row declares.
    from: Type,
    /// The payload type the destination row declares.
    to: Type,
}

impl Codegen<'_> {
    /// The rebuild carrying `from` into `to`, or `None` when the two rows share
    /// a runtime form and the value passes through untouched.
    pub(in crate::codegen) fn widen_leaf(
        &mut self,
        from: Type,
        to: Type,
    ) -> Result<Option<Callable>, LlvmError> {
        if let Some(cached) = self.widen_leaves.get(&(from, to)) {
            return Ok(*cached);
        }
        let changed = self.changed_variants(from, to)?;
        if changed.is_empty() {
            self.widen_leaves.insert((from, to), None);
            return Ok(None);
        }

        let (leaf, entry) = self.declare_widen_leaf();
        // Cached **before** the body is emitted: a template whose payload is an
        // instantiation of itself asks for this same leaf while emitting it, and
        // the declaration is what it must find.
        self.widen_leaves.insert((from, to), Some(leaf));

        // SAFETY: the builder is live wherever a widening is asked for, or null
        // when it is not inside a body yet; `entry` belongs to `leaf`.
        let resume = unsafe { LLVMGetInsertBlock(self.builder) };
        // SAFETY: `entry` is an empty block of a function in this module.
        unsafe { LLVMPositionBuilderAtEnd(self.builder, entry) };
        let emitted = self.emit_widen_body(leaf, from, &changed);
        // SAFETY: `resume` is the block the caller was building into, or null
        // when there was none.
        unsafe {
            if !resume.is_null() {
                LLVMPositionBuilderAtEnd(self.builder, resume);
            }
        }
        emitted?;
        Ok(Some(leaf))
    }

    /// The variants whose payload type differs between the two rows.
    ///
    /// Empty means the two rows have identical variants, which happens whenever
    /// a template never puts its parameter in a payload — `enum Marker<T> { On
    /// Off }` instantiated twice. Such a value needs no rebuild at all.
    fn changed_variants(&self, from: Type, to: Type) -> Result<Vec<ChangedVariant>, LlvmError> {
        let (Type::Enum(from_id), Type::Enum(to_id)) = (from, to) else {
            return Err(LlvmError::Unsupported(
                "a widening of something not an enum",
            ));
        };
        let (from_def, to_def) = (self.enum_def(from_id)?, self.enum_def(to_id)?);
        if from_def.variants.len() != to_def.variants.len() {
            return Err(LlvmError::Unsupported(
                "a widening between rows with different variants",
            ));
        }
        let mut changed = Vec::new();
        for (tag, (source, target)) in from_def
            .variants
            .iter()
            .zip(to_def.variants.iter())
            .enumerate()
        {
            match (source.payload, target.payload) {
                (Some(source_ty), Some(target_ty)) if source_ty != target_ty => {
                    changed.push(ChangedVariant {
                        tag: tag as u32,
                        from: source_ty,
                        to: target_ty,
                    });
                }
                (None, None) | (Some(_), Some(_)) => {}
                _ => {
                    return Err(LlvmError::Unsupported(
                        "a widening between rows whose variants disagree about a payload",
                    ));
                }
            }
        }
        Ok(changed)
    }

    /// Declares one rebuild and gives back its empty entry block.
    fn declare_widen_leaf(&mut self) -> (Callable, LLVMBasicBlockRef) {
        let name = c_string(&format!("kira.widen.{}", self.widen_leaves.len()));
        let mut params = [self.types.ptr];
        // SAFETY: every type belongs to this module's context, `params` outlives
        // the `LLVMFunctionType` call, and the block is appended to the function
        // just created.
        unsafe {
            let signature = LLVMFunctionType(self.types.ptr, params.as_mut_ptr(), 1, 0);
            let value = LLVMAddFunction(self.module, name.as_ptr(), signature);
            // Internal: a rebuild is this module's own, never part of its ABI.
            LLVMSetLinkage(value, llvm_sys::LLVMLinkage::LLVMInternalLinkage);
            let entry = LLVMAppendBasicBlockInContext(self.context, value, c"entry".as_ptr());
            (
                Callable {
                    ty: signature,
                    value,
                },
                entry,
            )
        }
    }

    /// The body: switch on the tag, rebuild the variants that changed, and hand
    /// every other one straight back.
    fn emit_widen_body(
        &mut self,
        leaf: Callable,
        from: Type,
        changed: &[ChangedVariant],
    ) -> Result<(), LlvmError> {
        // SAFETY: the parameter exists — the signature was just built with it —
        // and the builder is positioned on the entry block.
        let source = unsafe { LLVMGetParam(leaf.value, 0) };
        let tag = self.call(self.runtime.enum_tag, &mut [source], c"widen.tag");
        // SAFETY: `leaf.value` is a live function in this module's context.
        let unchanged = unsafe {
            LLVMAppendBasicBlockInContext(self.context, leaf.value, c"widen.same".as_ptr())
        };
        // SAFETY: `tag` is the `i64` the runtime returned and `unchanged` belongs
        // to this function.
        let switch = unsafe { LLVMBuildSwitch(self.builder, tag, unchanged, changed.len() as u32) };

        for variant in changed {
            let name = c_string(&format!("widen.tag.{}", variant.tag));
            // SAFETY: the function is live and the case value is this context's
            // `i64`, matching the switch's operand.
            unsafe {
                let block = LLVMAppendBasicBlockInContext(self.context, leaf.value, name.as_ptr());
                LLVMAddCase(switch, self.const_int(i64::from(variant.tag)), block);
                LLVMPositionBuilderAtEnd(self.builder, block);
            }
            // Read the payload *owned* before the source box is released, the
            // same order `lower_enum_payload` reads one in.
            let payload = self.read_box_payload(source, variant.from)?;
            let carried = self.widen_payload(payload, variant.from, variant.to)?;
            let tag_value = self.const_int(i64::from(variant.tag));
            let rebuilt = self.box_new(tag_value, variant.to, carried, c"widen.box")?;
            // The rebuild consumes its argument, so the source box goes here —
            // after everything has been read out of it.
            self.drop_value(source, from)?;
            // SAFETY: the builder is on the (unterminated) block `drop_value`
            // left it on, and `rebuilt` has this function's return type.
            unsafe { LLVMBuildRet(self.builder, rebuilt) };
        }

        // SAFETY: `unchanged` is an empty block of this function, and `source`
        // is its parameter — ownership passes straight through.
        unsafe {
            LLVMPositionBuilderAtEnd(self.builder, unchanged);
            LLVMBuildRet(self.builder, source);
        }
        Ok(())
    }

    /// Carries one payload value from the source row's type to the destination
    /// row's.
    ///
    /// Exactly the two crossings the type rule admits — into the top type, or
    /// into another instantiation — which is what makes the `Unsupported` arm
    /// unreachable rather than a gap: `TypeTable::admits` refused everything
    /// else before this ran.
    fn widen_payload(
        &mut self,
        value: LLVMValueRef,
        from: Type,
        to: Type,
    ) -> Result<LLVMValueRef, LlvmError> {
        if from == to {
            return Ok(value);
        }
        if to == Type::Any {
            return self.erase_value(value, from);
        }
        if !matches!((from, to), (Type::Enum(_), Type::Enum(_))) {
            return Err(LlvmError::Unsupported(
                "a widening of a payload the type rule refuses",
            ));
        }
        match self.widen_leaf(from, to)? {
            Some(leaf) => Ok(self.call(leaf, &mut [value], c"widen.nested")),
            None => Ok(value),
        }
    }

    /// The definition behind an enum id.
    fn enum_def(&self, id: EnumId) -> Result<&kira_semantics_model::EnumDef, LlvmError> {
        self.program
            .types
            .enums()
            .get(id)
            .ok_or(LlvmError::Unsupported("an enum the program never declared"))
    }
}
