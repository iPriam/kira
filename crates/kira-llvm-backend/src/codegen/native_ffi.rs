//! Native constants for the shared recursive libffi descriptor graph.

use kira_runtime_abi::{ForeignArrayElement, ForeignMember, ForeignTypeSpec};
use llvm_sys::core::*;
use llvm_sys::prelude::*;
use llvm_sys::{LLVMLinkage, LLVMUnnamedAddr};

use super::Codegen;
use super::ffi::c_string;
use crate::LlvmError;

const MAGIC: u32 = 0x3146_464b;

impl Codegen<'_> {
    /// Emits one private descriptor blob per import and per callback.
    pub(super) fn declare_foreign_ffi_descriptors(&mut self) -> Result<(), LlvmError> {
        for (index, entry) in self.program.foreign_imports.iter().enumerate() {
            let bytes =
                descriptor_bytes(entry.import.signature(), &self.program.foreign_aggregates)?;
            let global = self.descriptor_global(&format!("kira.ffi.descriptor.{index}"), &bytes);
            self.foreign_ffi_descriptors.push(global);
        }
        for index in 0..self.program.foreign_callbacks.len() {
            let signature = self.program.foreign_callbacks[index].signature().clone();
            let bytes = descriptor_bytes(&signature, &self.program.foreign_aggregates)?;
            let global =
                self.descriptor_global(&format!("kira.ffi.callback.descriptor.{index}"), &bytes);
            self.callback_ffi_descriptors.push(global);
        }
        Ok(())
    }

    /// Interns one descriptor blob as a private constant.
    fn descriptor_global(&mut self, name: &str, bytes: &[u8]) -> LLVMValueRef {
        let name = c_string(name);
        // SAFETY: all values and types belong to this live LLVM context; LLVM
        // copies the constant array during each call.
        unsafe {
            let values: Vec<LLVMValueRef> = bytes
                .iter()
                .map(|byte| LLVMConstInt(self.types.i8, u64::from(*byte), 0))
                .collect();
            let array = LLVMConstArray2(
                self.types.i8,
                values.as_ptr().cast_mut(),
                values.len() as u64,
            );
            let array_type = LLVMArrayType2(self.types.i8, values.len() as u64);
            let global = LLVMAddGlobal(self.module, array_type, name.as_ptr());
            LLVMSetInitializer(global, array);
            LLVMSetGlobalConstant(global, 1);
            LLVMSetLinkage(global, LLVMLinkage::LLVMPrivateLinkage);
            LLVMSetUnnamedAddress(global, LLVMUnnamedAddr::LLVMGlobalUnnamedAddr);
            global
        }
    }

    /// Returns a callback's descriptor global as an opaque byte pointer.
    pub(super) fn callback_ffi_descriptor(&self, index: usize) -> Result<LLVMValueRef, LlvmError> {
        let global = self
            .callback_ffi_descriptors
            .get(index)
            .copied()
            .ok_or(LlvmError::internal("a callback has no libffi descriptor"))?;
        // SAFETY: a global's address is a pointer in this module and the opaque
        // pointer type belongs to the same context.
        Ok(unsafe { LLVMConstBitCast(global, self.types.ptr) })
    }

    /// Returns a descriptor global as an opaque byte pointer.
    pub(super) fn foreign_ffi_descriptor(&self, index: usize) -> Result<LLVMValueRef, LlvmError> {
        let global =
            self.foreign_ffi_descriptors
                .get(index)
                .copied()
                .ok_or(LlvmError::internal(
                    "a foreign import has no libffi descriptor",
                ))?;
        // SAFETY: a global's address is a pointer in this module and the opaque
        // pointer type belongs to the same context.
        Ok(unsafe { LLVMConstBitCast(global, self.types.ptr) })
    }
}

fn descriptor_bytes(
    signature: &kira_runtime_abi::ForeignSignature,
    aggregates: &kira_runtime_abi::ForeignAggregates,
) -> Result<Vec<u8>, LlvmError> {
    let mut words = Vec::new();
    words.push(MAGIC);
    words.push(0);
    words.push(
        u32::try_from(signature.parameters().len())
            .map_err(|_| LlvmError::internal("a foreign signature has too many parameters"))?,
    );
    push_spec(&mut words, signature.result());
    words.push(
        u32::try_from(aggregates.len())
            .map_err(|_| LlvmError::internal("a foreign aggregate table is too large"))?,
    );
    for spec in signature.parameters() {
        push_spec(&mut words, *spec);
    }
    for aggregate in aggregates.iter() {
        words.push(
            u32::try_from(aggregate.members().len())
                .map_err(|_| LlvmError::internal("a foreign aggregate has too many members"))?,
        );
        for member in aggregate.members() {
            match member {
                ForeignMember::Scalar(ty) => {
                    words.extend([0, u32::from(ty.tag()), 0, 0]);
                }
                ForeignMember::Aggregate(id) => {
                    words.extend([1, 0, id.0, 0]);
                }
                ForeignMember::Array { element, count } => match element {
                    ForeignArrayElement::Scalar(ty) => {
                        words.extend([2, u32::from(ty.tag()), u32::MAX, *count]);
                    }
                    ForeignArrayElement::Aggregate(id) => {
                        words.extend([2, 0, id.0, *count]);
                    }
                },
            }
        }
    }
    let byte_count = words
        .len()
        .checked_mul(4)
        .ok_or(LlvmError::internal("a foreign descriptor is too large"))?;
    let byte_count = u32::try_from(byte_count)
        .map_err(|_| LlvmError::internal("a foreign descriptor is too large"))?;
    words[1] = byte_count;
    let mut bytes = Vec::with_capacity(byte_count as usize);
    for word in words {
        bytes.extend_from_slice(&word.to_le_bytes());
    }
    Ok(bytes)
}

fn push_spec(words: &mut Vec<u32>, spec: ForeignTypeSpec) {
    match spec {
        ForeignTypeSpec::Scalar(ty) => words.extend([0, u32::from(ty.tag()), 0]),
        ForeignTypeSpec::Aggregate(id) => words.extend([1, 0, id.0]),
    }
}
