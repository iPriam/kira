//! Packing and unpacking `BridgeValue`s: how a value crosses between the two
//! engines of a hybrid program.
//!
//! Both directions of the boundary meet here. A trampoline *reads* the args the
//! host packed and *writes* the result back; a native-to-runtime call does the
//! mirror. One pair of routines serves both, so the two directions cannot
//! disagree about the layout.

use kira_runtime_abi::BridgeValueTag;
use kira_semantics_model::Type;
use llvm_sys::core::*;
use llvm_sys::prelude::*;

use super::Codegen;
use crate::LlvmError;

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
        Type::Int(_) => (BridgeValueTag::INT.0, Some(PayloadForm::AsIs)),
        Type::Float(_) => (BridgeValueTag::FLOAT.0, Some(PayloadForm::FloatBits)),
        Type::Bool => (BridgeValueTag::BOOL.0, Some(PayloadForm::Widen)),
        Type::String => (BridgeValueTag::STRING.0, Some(PayloadForm::PointerBits)),
        // A `BridgeValue` is 16 bytes with a one-word payload; a struct does
        // not fit and has no tag. Crossing the seam with one needs an ABI
        // decision (by value? by pointer? who frees the strings inside?) that
        // has not been made, so the boundary says no rather than guessing.
        Type::Struct(_) => return Err(LlvmError::StructAtSeam),
        // An array does not fit either, but the reason is different: the
        // language does let one cross, and what is missing is the ownership
        // answer at the boundary — who frees the elements, and what a native
        // callee growing the array means for the other half. See
        // `BridgeValueTag::ARRAY`.
        Type::Array(_) => return Err(LlvmError::ArrayAtSeam),
        // An enum does not fit either, and on the same grounds as a struct: it
        // is a tagged value with no one-word form, and how it would cross is
        // undecided. See `BridgeValueTag::ENUM`.
        Type::Enum(_) => return Err(LlvmError::EnumAtSeam),
        // A `RawPtr` and a `CString` are foreign-seam values, not `@Native`-seam
        // ones: the VM refuses both here (`Heap::lower`/`lift` return `None`), so
        // native code refuses them too, keeping the two engines in agreement. A
        // raw pointer crosses the *foreign* seam through a generated adapter, and
        // a `CString` is a foreign parameter position that never becomes a value.
        Type::RawPtr | Type::CString => {
            return Err(LlvmError::Unsupported(
                "a raw pointer or C string crossing the @Native boundary",
            ));
        }
        Type::Error => return Err(LlvmError::Unsupported("a value with no type")),
    })
}

impl Codegen<'_> {
    /// Reads one `BridgeValue`'s payload as a value of type `ty`.
    ///
    /// The tag is not consulted: the static type is what the manifest promised
    /// and what the other side encoded from. The tag exists so a *reader* that
    /// does not know the signature can still refuse an unknown value.
    pub(super) fn read_bridge_payload(
        &self,
        slot: LLVMValueRef,
        ty: Type,
    ) -> Result<LLVMValueRef, LlvmError> {
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
                Type::Int(_) => payload,
                Type::Float(_) => {
                    LLVMBuildBitCast(self.builder, payload, types.f64, c"arg.float".as_ptr())
                }
                Type::Bool => LLVMBuildTrunc(self.builder, payload, types.i1, c"arg.bool".as_ptr()),
                Type::String => {
                    LLVMBuildIntToPtr(self.builder, payload, types.ptr, c"arg.str".as_ptr())
                }
                Type::Struct(_) => return Err(LlvmError::StructAtSeam),
                Type::Array(_) => return Err(LlvmError::ArrayAtSeam),
                Type::Enum(_) => return Err(LlvmError::EnumAtSeam),
                Type::RawPtr | Type::CString => {
                    return Err(LlvmError::Unsupported(
                        "a raw pointer or C string crossing the @Native boundary",
                    ));
                }
                Type::Void | Type::Error => {
                    return Err(LlvmError::Unsupported("a parameter with no runtime value"));
                }
            })
        }
    }

    /// Writes `value` into the `BridgeValue` at `slot`, tagged for `ty`.
    ///
    /// The write itself goes through [`Codegen::store_bridge`], the one routine
    /// that lays down all three fields — tag, zeroed reserved bytes, payload — so
    /// the `@Native` seam and the foreign seam cannot drift on the must-be-zero
    /// reserved invariant. This routine's only added job is turning a typed
    /// `value` into the one payload word `ty` encodes into.
    pub(super) fn write_bridge_value(
        &self,
        slot: LLVMValueRef,
        value: LLVMValueRef,
        ty: Type,
    ) -> Result<(), LlvmError> {
        let types = self.types;
        let (tag, payload) = bridge_tag_of(ty)?;
        // SAFETY: `value` has `ty`'s LLVM type and the builder is on a live block.
        let payload = unsafe {
            match payload {
                // Void carries no payload; an explicit zero word keeps it defined.
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
            }
        };
        self.store_bridge(slot, tag, payload);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use kira_runtime_abi::Execution;
    use kira_semantics_model::Type;
    use kira_semantics_model::hir::{HirExpr, HirFunction, HirProgram, HirStmt};
    use kira_source::Span;

    use crate::codegen::Module;

    /// A `@Native` function's host trampoline packs its return value into a
    /// `BridgeValue` that lives in an alloca, so the reserved gap starts as stack
    /// garbage. The must-be-zero reserved invariant only holds if the trampoline
    /// zeroes it before the value crosses `extern "C"`. `write_bridge_value`
    /// writes through `store_bridge`, the one routine that zeroes reserved; this
    /// pins that the `@Native` seam actually emits that store.
    #[test]
    fn a_native_trampoline_zeroes_the_bridge_value_reserved_bytes() {
        let mut program = HirProgram::default();
        let value = program.exprs.alloc(HirExpr::Int(42));
        let ret = program.stmts.alloc(HirStmt::Return { value: Some(value) });
        program.functions.push(HirFunction {
            name: "answer".to_owned(),
            param_count: 0,
            return_type: Type::INT,
            locals: Vec::new(),
            body: vec![ret],
            is_main: false,
            execution: Execution::Native,
            mutates_self: false,
            name_span: Span::new(0, 6),
        });
        let ir = kira_ir::lower(&program);

        let module = Module::build_hybrid(&ir, "reserved_probe").expect("the hybrid half builds");
        let dir = std::env::temp_dir().join(format!("kira-bridge-reserved-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("the temp dir is creatable");
        let ir_path = dir.join("reserved_probe.ll");
        module.write_ir(&ir_path).expect("the IR is emitted");
        let text = std::fs::read_to_string(&ir_path).expect("the IR is readable");
        let _ = std::fs::remove_dir_all(&dir);

        // `LLVMConstNull` of `[7 x i8]` prints as `zeroinitializer`; its store is
        // the reserved-bytes write the trampoline must emit.
        assert!(
            text.contains("store [7 x i8] zeroinitializer"),
            "the @Native trampoline must zero the BridgeValue reserved bytes; IR:\n{text}"
        );
    }
}
