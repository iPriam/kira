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
    /// A payload-less enum's variant tag, shifted out of its inline handle.
    ///
    /// Such an enum is never allocated: its handle carries the tag directly as
    /// `(tag << 1) | 1`, which the runtime recognizes by the low bit. So the
    /// payload is neither the value's bits nor something loaded from it — the
    /// handle is decoded on the way out and re-encoded on the way in, and the
    /// far side, which keeps its enums somewhere else entirely, sees only a
    /// variant number.
    EnumTag,
    /// An aggregate as a pointer to a native-value node tree.
    ///
    /// What a struct, an array, and an enum carrying a payload all cross as.
    /// The tree is built here and transferred; the reader frees it. See
    /// [`kira_runtime_abi::BridgeValueTag::NODE`].
    Node,
}

impl Codegen<'_> {
    /// The bridge tag for `ty`, and how its payload is encoded.
    fn bridge_tag_of(&self, ty: Type) -> Result<(u8, Option<PayloadForm>), LlvmError> {
        bridge_tag_of(ty, self.program.types.enums())
    }
}

/// The bridge tag for `ty`, and how its payload is encoded.
fn bridge_tag_of(
    ty: Type,
    enums: &kira_semantics_model::EnumTable,
) -> Result<(u8, Option<PayloadForm>), LlvmError> {
    Ok(match ty {
        Type::Void => (BridgeValueTag::VOID.0, None),
        Type::Int(_) => (BridgeValueTag::INT.0, Some(PayloadForm::AsIs)),
        Type::Float(_) => (BridgeValueTag::FLOAT.0, Some(PayloadForm::FloatBits)),
        Type::Bool => (BridgeValueTag::BOOL.0, Some(PayloadForm::Widen)),
        Type::String => (BridgeValueTag::STRING.0, Some(PayloadForm::PointerBits)),
        Type::RawPtr | Type::ForeignPtr(_) => (BridgeValueTag::RAW_PTR.0, Some(PayloadForm::AsIs)),
        // A struct and an array both cross as a node tree. Neither fits one
        // word, and neither side's storage means anything to the other — the VM
        // holds an index into its heap, native a pointer to a box — so what
        // crosses is a copy in the `kira_rt_native_value_*` form both can build
        // and read, transferred to the reader, who frees it as it decodes.
        // That is the answer to who frees the strings inside: the side that
        // reads them, exactly once. See `BridgeValueTag::NODE`.
        Type::Struct(_) | Type::Array(_) => (BridgeValueTag::NODE.0, Some(PayloadForm::Node)),
        // A payload-less enum *is* its tag, so the tag crosses and the far side
        // rebuilds its own value from it: nothing is owned, nothing is freed,
        // and neither side's representation travels. One carrying a payload is
        // a tag plus something owned, which does not fit one word. See
        // `BridgeValueTag::ENUM`.
        Type::Enum(id) if enums.is_fieldless(id) => {
            (BridgeValueTag::ENUM.0, Some(PayloadForm::EnumTag))
        }
        // One carrying a payload takes the node tree instead: the tag alone
        // would lose whatever the variant holds, and a tree carries both.
        Type::Enum(_) => (BridgeValueTag::NODE.0, Some(PayloadForm::Node)),
        // `RawPtr` crosses this seam for opaque callback userdata. `CString`
        // remains foreign-parameter-only, and a state handle itself stays in the
        // engine that owns the intrinsic; only its raw token crosses.
        Type::CString
        | Type::CBlock
        | Type::NativeState(_)
        | Type::Task(_)
        | Type::MainThreadTask(_)
        | Type::Cell(_) => {
            return Err(LlvmError::internal(
                "a C string, callback-state handle, task handle, or captured `var` crossing the @Native boundary",
            ));
        }
        // An erased value carries its dynamic identity and payload in a node
        // tree. The bridge tag distinguishes that root from an ordinary
        // aggregate, while the node retains the recursively owned value.
        Type::Any => (BridgeValueTag::ANY.0, Some(PayloadForm::Node)),
        Type::Error => return Err(LlvmError::internal("a value with no type")),
        // `kira-ir` rewrites every `distinct` type to the scalar it is before
        // a backend sees the program, so what crosses this seam is that
        // scalar's tag. One reaching here is a lowering that skipped the
        // erasure, which is a broken contract rather than a value to guess a
        // tag for.
        Type::Distinct(_) => {
            return Err(LlvmError::internal(
                "a distinct type that lowering did not erase",
            ));
        }
    })
}

impl Codegen<'_> {
    /// Reads one `BridgeValue`'s payload as a value of type `ty`.
    ///
    /// The tag is not consulted: the static type is what the manifest promised
    /// and what the other side encoded from. The tag exists so a *reader* that
    /// does not know the signature can still refuse an unknown value.
    pub(super) fn read_bridge_payload(
        &mut self,
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
                // The payload is a node tree the other side built and handed
                // over. Decoding consumes it: `decode_native_state_value`
                // frees each node as it reads it, which is what makes the
                // transfer exactly one free rather than two or none.
                Type::Struct(_) | Type::Array(_) => {
                    let node =
                        LLVMBuildIntToPtr(self.builder, payload, types.ptr, c"arg.node".as_ptr());
                    self.decode_native_state_value(node, ty)?
                }
                // A payload-less enum is never allocated here: its handle *is*
                // the tag, inline as `(tag << 1) | 1`, which the runtime
                // recognizes by the low bit and treats as clone-is-identity,
                // free-is-nothing. So the arriving tag is re-encoded rather
                // than boxed — the same word `lower_enum_new` builds for a
                // constant, built for a value that is only known now.
                Type::Enum(id) if self.program.types.enums().is_fieldless(id) => {
                    self.inline_enum_value(payload)
                }
                // A payload-carrying enum arrives as a tree, for the same
                // reason a struct does: the tag alone would drop the payload.
                Type::Enum(_) => {
                    let node =
                        LLVMBuildIntToPtr(self.builder, payload, types.ptr, c"arg.node".as_ptr());
                    self.decode_native_state_value(node, ty)?
                }
                Type::Any => {
                    let node =
                        LLVMBuildIntToPtr(self.builder, payload, types.ptr, c"arg.any".as_ptr());
                    self.decode_native_state_value(node, ty)?
                }
                Type::RawPtr | Type::ForeignPtr(_) => payload,
                Type::MainThreadTask(_) => payload,
                Type::CString
                | Type::CBlock
                | Type::NativeState(_)
                | Type::Task(_)
                | Type::Cell(_) => {
                    return Err(LlvmError::internal(
                        "a C string, callback-state handle, task handle, or captured `var` crossing the @Native boundary",
                    ));
                }
                Type::Void | Type::Error => {
                    return Err(LlvmError::internal("a parameter with no runtime value"));
                }
                Type::Distinct(_) => {
                    return Err(LlvmError::internal(
                        "a distinct type that lowering did not erase",
                    ));
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
        &mut self,
        slot: LLVMValueRef,
        value: LLVMValueRef,
        ty: Type,
    ) -> Result<(), LlvmError> {
        let types = self.types;
        let (tag, payload) = self.bridge_tag_of(ty)?;
        // Built before the `unsafe` block below because encoding a tree emits
        // calls and needs `&mut self`, which the raw builder sequence does not
        // give up. The result is one pointer, which that sequence then widens
        // exactly as it widens any other.
        let node = match payload {
            Some(PayloadForm::Node) => Some(self.encode_native_state_value(value, ty)?),
            _ => None,
        };
        // Read through the runtime's own accessor, which is the one place that
        // knows how an enum handle is shaped: it tests the low bit and answers
        // from the handle when the value is inline, or from the box when it is
        // not. Every `match` reads a tag the same way.
        //
        // Shifting the handle by hand would work for the inline case and answer
        // *garbage* for a boxed one — a wrong tag is a wrong variant, which is a
        // different program rather than a crash. An earlier version did the
        // opposite, loading through the handle as if it were always a box, and
        // segfaulted on address 3.
        let enum_tag = match payload {
            Some(PayloadForm::EnumTag) => {
                self.call(self.runtime.enum_tag, &mut [value], c"ret.enum.tag")
            }
            _ => std::ptr::null_mut(),
        };
        // SAFETY: `value` has `ty`'s LLVM type and the builder is on a live block.
        let payload = unsafe {
            match payload {
                // The tree is transferred: nothing is freed here, and the
                // reader's decode is the one free.
                Some(PayloadForm::Node) => match node {
                    Some(node) => {
                        LLVMBuildPtrToInt(self.builder, node, types.i64, c"ret.node".as_ptr())
                    }
                    None => LLVMConstInt(types.i64, 0, 0),
                },
                // Void carries no payload; an explicit zero word keeps it defined.
                None => LLVMConstInt(types.i64, 0, 0),
                Some(PayloadForm::AsIs) => value,
                // The handle *is* the tag, inline as `(tag << 1) | 1`, so the
                // tag comes back out by shifting rather than by loading: a
                // payload-less enum is never allocated, and dereferencing the
                // handle would read address `(tag << 1) | 1` — which is how
                // this first crashed, on address 3.
                Some(PayloadForm::EnumTag) => enum_tag,
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
    use kira_semantics_model::hir::CallableSignature;
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
            is_main_thread: false,
            is_async: false,
            execution: Execution::Native,
            mutates_self: false,
            name_span: Span::new(0, 6),
            signature: CallableSignature::synthesized(&[], Type::INT),
        });
        let ir = kira_ir::lower(&program);

        let module =
            Module::build_hybrid(&ir, "reserved_probe", &[]).expect("the hybrid half builds");
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

    #[test]
    fn a_hybrid_app_omits_an_unreachable_native_function() {
        let mut program = HirProgram::default();
        let main_value = program.exprs.alloc(HirExpr::Int(0));
        let main_return = program.stmts.alloc(HirStmt::Return {
            value: Some(main_value),
        });
        program.functions.push(HirFunction {
            name: "main".to_owned(),
            param_count: 0,
            return_type: Type::INT,
            locals: Vec::new(),
            body: vec![main_return],
            is_main: true,
            is_main_thread: false,
            is_async: false,
            execution: Execution::Runtime,
            mutates_self: false,
            name_span: Span::new(0, 4),
            signature: CallableSignature::synthesized(&[], Type::INT),
        });
        let unused_value = program.exprs.alloc(HirExpr::Int(7));
        let unused_return = program.stmts.alloc(HirStmt::Return {
            value: Some(unused_value),
        });
        program.functions.push(HirFunction {
            name: "unused_native".to_owned(),
            param_count: 0,
            return_type: Type::INT,
            locals: Vec::new(),
            body: vec![unused_return],
            is_main: false,
            is_main_thread: false,
            is_async: false,
            execution: Execution::Native,
            mutates_self: false,
            name_span: Span::new(5, 17),
            signature: CallableSignature::synthesized(&[], Type::INT),
        });
        program.main = Some(kira_semantics_model::hir::FuncId(0));
        let ir = kira_ir::lower(&program);

        let module =
            Module::build_hybrid(&ir, "unreachable_native", &[]).expect("the hybrid half builds");
        let directory =
            std::env::temp_dir().join(format!("kira-hybrid-reachability-{}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("the temp directory is creatable");
        let ir_path = directory.join("unreachable_native.ll");
        module.write_ir(&ir_path).expect("the IR is emitted");
        let text = std::fs::read_to_string(&ir_path).expect("the IR is readable");
        let _ = std::fs::remove_dir_all(&directory);

        assert!(
            !text.contains("kira_fn_1_unused_native"),
            "an unreachable native body was emitted; IR:\n{text}"
        );
    }
}
