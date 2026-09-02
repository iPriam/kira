//! What a runtime type descriptor answers on the native side.
//!
//! The VM reads the descriptor table its module carries. Native code has no
//! module to read, so the table becomes code: one generated function per
//! property, switching on the descriptor id and answering with a constant.
//! Nothing new crosses the runtime ABI for this — a name is
//! `kira_rt_str_new` over a string constant, and the arguments are the array
//! any Kira array literal builds — which is what keeps the two engines'
//! answers one design rather than two.
//!
//! The switch is over the ids the program actually interned, so a program that
//! asks nothing generates nothing, and one that asks generates exactly the rows
//! it can reach.

use kira_semantics_model::{ErasedTypeId, Type, TypeField};
use llvm_sys::core::*;
use llvm_sys::prelude::*;
use llvm_sys::LLVMIntPredicate;

use super::{Callable, Codegen};
use crate::LlvmError;

impl<'a> Codegen<'a> {
    /// The generated reader for `field`, built once per module.
    pub(in crate::codegen) fn type_field_reader(
        &mut self,
        field: TypeField,
    ) -> Result<Callable, LlvmError> {
        if let Some(&cached) = self.type_field_readers.get(&field) {
            return Ok(cached);
        }
        let (callable, entry) = self.declare_type_field_reader(field);
        self.type_field_readers.insert(field, callable);
        // SAFETY: the builder is live for the whole of code generation, and the
        // block belongs to the function just declared.
        let restore = unsafe {
            let restore = LLVMGetInsertBlock(self.builder);
            LLVMPositionBuilderAtEnd(self.builder, entry);
            restore
        };
        let emitted = self.emit_type_field_reader(field, callable);
        // SAFETY: `restore` is the block the caller was building into, or null
        // when there was none.
        unsafe {
            if !restore.is_null() {
                LLVMPositionBuilderAtEnd(self.builder, restore);
            }
        }
        emitted?;
        Ok(callable)
    }

    /// Declares `kira.type.<field>(i64) -> ptr`.
    fn declare_type_field_reader(&mut self, field: TypeField) -> (Callable, LLVMBasicBlockRef) {
        let name = match field {
            TypeField::Name => c"kira.type.name",
            TypeField::Package => c"kira.type.package",
            TypeField::Kind => c"kira.type.kind",
            TypeField::Arguments => c"kira.type.arguments",
            TypeField::Conformances => c"kira.type.conformances",
        };
        // SAFETY: the module and context are live, and the signature is the one
        // every call site builds.
        unsafe {
            let mut params = [self.types.i64];
            let signature = LLVMFunctionType(self.types.ptr, params.as_mut_ptr(), 1, 0);
            let function = LLVMAddFunction(self.module, name.as_ptr(), signature);
            LLVMSetLinkage(function, llvm_sys::LLVMLinkage::LLVMInternalLinkage);
            let entry = LLVMAppendBasicBlockInContext(self.context, function, c"entry".as_ptr());
            (
                Callable {
                    value: function,
                    ty: signature,
                },
                entry,
            )
        }
    }

    /// Emits the body: one comparison per descriptor, answering with its row.
    ///
    /// A chain rather than a `switch` because each arm builds a value — a
    /// string or an array — and returns it, so there is nothing to merge.
    fn emit_type_field_reader(
        &mut self,
        field: TypeField,
        callable: Callable,
    ) -> Result<(), LlvmError> {
        // SAFETY: the function was just declared with one parameter.
        let asked = unsafe { LLVMGetParam(callable.value, 0) };
        let rows: Vec<(u64, u32)> = self
            .program
            .descriptors
            .interned()
            .into_iter()
            .filter_map(|(ty, index)| {
                let id = ErasedTypeId::known(&self.program.descriptors, ty)?;
                Some((id.as_u64(), index))
            })
            .collect();
        for (id, index) in rows {
            // SAFETY: every value belongs to this module's context, and the
            // builder sits on a live block of this function.
            let next = unsafe {
                let expected = LLVMConstInt(self.types.i64, id, 0);
                let matches = LLVMBuildICmp(
                    self.builder,
                    LLVMIntPredicate::LLVMIntEQ,
                    asked,
                    expected,
                    c"type.field.is".as_ptr(),
                );
                let hit =
                    LLVMAppendBasicBlockInContext(self.context, callable.value, c"hit".as_ptr());
                let next =
                    LLVMAppendBasicBlockInContext(self.context, callable.value, c"next".as_ptr());
                LLVMBuildCondBr(self.builder, matches, hit, next);
                LLVMPositionBuilderAtEnd(self.builder, hit);
                next
            };
            let answer = self.type_field_value(field, index)?;
            // SAFETY: the builder is on the arm's block and `answer` is its
            // result.
            unsafe {
                LLVMBuildRet(self.builder, answer);
                LLVMPositionBuilderAtEnd(self.builder, next);
            }
        }
        // An id no row claims: empty text, or no arguments. Unreachable for a
        // program the compiler built, and an answer rather than a trap for one
        // whose table a loader truncated.
        let empty = self.empty_type_field_value(field)?;
        // SAFETY: the builder sits on the fallthrough block.
        unsafe { LLVMBuildRet(self.builder, empty) };
        Ok(())
    }

    /// The value one row answers `field` with.
    fn type_field_value(
        &mut self,
        field: TypeField,
        index: u32,
    ) -> Result<LLVMValueRef, LlvmError> {
        let row = self
            .program
            .descriptors
            .get(index)
            .ok_or(LlvmError::internal("a descriptor row the table never held"))?;
        let text = match field {
            TypeField::Name => row.name.clone(),
            TypeField::Package => row.package.clone(),
            TypeField::Kind => row.kind.label().to_owned(),
            TypeField::Arguments => {
                let arguments: Vec<u64> = row
                    .arguments
                    .iter()
                    .filter_map(|&argument| {
                        let family = self.program.descriptors.get(argument)?.family;
                        Some(ErasedTypeId::from_parts(family, argument).as_u64())
                    })
                    .collect();
                return self.type_argument_array(&arguments);
            }
            TypeField::Conformances => {
                let names = row.conformances.clone();
                return self.kira_string_array(&names);
            }
        };
        Ok(self.kira_string(&text))
    }

    /// The answer for an id no row claims.
    fn empty_type_field_value(&mut self, field: TypeField) -> Result<LLVMValueRef, LlvmError> {
        match field {
            TypeField::Arguments => self.type_argument_array(&[]),
            TypeField::Conformances => self.kira_string_array(&[]),
            _ => Ok(self.kira_string("")),
        }
    }

    /// A Kira `[String]` holding `names`.
    fn kira_string_array(&mut self, names: &[String]) -> Result<LLVMValueRef, LlvmError> {
        let count = self.const_int(names.len() as i64);
        let element_size = self.abi_size(Type::String)?;
        let handle = self.call(
            self.runtime.array_new,
            &mut [count, element_size],
            c"type.conformances",
        );
        for (position, name) in names.iter().enumerate() {
            let text = self.kira_string(name);
            let at = self.const_int(position as i64);
            let element_size = self.abi_size(Type::String)?;
            let slot = self.call(
                self.runtime.array_slot,
                &mut [handle, at, element_size],
                c"type.conformance.slot",
            );
            // SAFETY: the slot addresses a `String` element of a fresh array
            // and the builder sits on a live block.
            unsafe { LLVMBuildStore(self.builder, text, slot) };
        }
        Ok(handle)
    }

    /// A Kira `String` holding `text`.
    fn kira_string(&mut self, text: &str) -> LLVMValueRef {
        let data = self.string_constant(text);
        let length = self.const_usize(text.len() as u64);
        self.call(self.runtime.str_new, &mut [data, length], c"type.text")
    }

    /// A Kira `[Type]` holding `ids`.
    fn type_argument_array(&mut self, ids: &[u64]) -> Result<LLVMValueRef, LlvmError> {
        let count = self.const_int(ids.len() as i64);
        let element_size = self.abi_size(Type::RuntimeType)?;
        let handle = self.call(
            self.runtime.array_new,
            &mut [count, element_size],
            c"type.arguments",
        );
        for (position, &id) in ids.iter().enumerate() {
            let at = self.const_int(position as i64);
            let element_size = self.abi_size(Type::RuntimeType)?;
            let slot = self.call(
                self.runtime.array_slot,
                &mut [handle, at, element_size],
                c"type.argument.slot",
            );
            // SAFETY: the slot addresses an `i64` element of a fresh array and
            // the builder sits on a live block.
            unsafe {
                let value = LLVMConstInt(self.types.i64, id, 0);
                LLVMBuildStore(self.builder, value, slot);
            }
        }
        Ok(handle)
    }
}
