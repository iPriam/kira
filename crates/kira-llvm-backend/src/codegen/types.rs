//! LLVM value types and target-dependent layout shapes.

use llvm_sys::core::*;
use llvm_sys::prelude::*;

use kira_runtime_abi::ForeignPointerWidth;

mod runtime;
mod runtime_declare;
pub(super) use runtime::Runtime;
pub(super) use runtime_declare::declare_runtime;

/// A callable LLVM value together with its function type.
///
/// Opaque pointers mean a function value no longer carries its signature, so
/// every call site needs the type back; keeping them paired makes that
/// impossible to get wrong.
#[derive(Clone, Copy)]
pub(crate) struct Callable {
    /// The function's type.
    pub(super) ty: LLVMTypeRef,
    /// The function value.
    pub(super) value: LLVMValueRef,
}

/// The LLVM types Kira's v0 value types map onto.
#[derive(Clone, Copy)]
pub(crate) struct Types {
    pub(super) void: LLVMTypeRef,
    pub(super) i1: LLVMTypeRef,
    pub(super) i8: LLVMTypeRef,
    pub(super) i16: LLVMTypeRef,
    pub(super) i32: LLVMTypeRef,
    pub(super) i64: LLVMTypeRef,
    /// A 32-bit IEEE float, used only at the foreign C boundary for `F32`.
    pub(super) f32: LLVMTypeRef,
    pub(super) f64: LLVMTypeRef,
    /// The opaque pointer every `String` handle is.
    pub(super) ptr: LLVMTypeRef,
    /// The target's `usize`, for runtime helpers that take one.
    pub(super) usize_ty: LLVMTypeRef,
    /// `BridgeValue`: `{ i8 tag, [7 x i8] reserved, i64 payload }`.
    pub(super) bridge_value: LLVMTypeRef,
    /// `KiraEnum`: `{ i64 tag, i64 payload_kind, i64 payload, usize shares }`.
    pub(super) enum_box: LLVMTypeRef,
    /// `KiraArray`: `{ usize len, usize cap, ptr items, usize shares }`.
    pub(super) array_header: LLVMTypeRef,
    /// `KiraString`: `{ ptr, usize len, usize shares }`.
    pub(super) string_box: LLVMTypeRef,
}

impl Types {
    /// Creates every type in `context`.
    pub(super) fn new(context: LLVMContextRef, pointer_width: ForeignPointerWidth) -> Types {
        // SAFETY: every type below is created in this live context.
        unsafe {
            let usize_ty = match pointer_width {
                ForeignPointerWidth::Bits32 => LLVMInt32TypeInContext(context),
                ForeignPointerWidth::Bits64 => LLVMInt64TypeInContext(context),
            };
            Types {
                usize_ty,
                void: LLVMVoidTypeInContext(context),
                i1: LLVMInt1TypeInContext(context),
                i8: LLVMInt8TypeInContext(context),
                i16: LLVMInt16TypeInContext(context),
                i32: LLVMInt32TypeInContext(context),
                i64: LLVMInt64TypeInContext(context),
                f32: LLVMFloatTypeInContext(context),
                f64: LLVMDoubleTypeInContext(context),
                ptr: LLVMPointerTypeInContext(context, 0),
                bridge_value: bridge_value_type(context),
                enum_box: enum_box_type(context, usize_ty),
                array_header: array_header_type(context, usize_ty),
                string_box: string_box_type(context, usize_ty),
            }
        }
    }
}

/// The LLVM form of `kira_native_bridge::enums::KiraEnum`.
///
/// `{ i64, i64, i64, usize }` — the share count last, where that crate's layout
/// test puts it, so the three fields before it keep the offsets they had when
/// the box carried no count at all.
fn enum_box_type(context: LLVMContextRef, usize_ty: LLVMTypeRef) -> LLVMTypeRef {
    // SAFETY: every type is created in this live context; `fields` outlives the
    // struct-type call.
    unsafe {
        let i64_ty = LLVMInt64TypeInContext(context);
        let mut fields = [i64_ty, i64_ty, i64_ty, usize_ty];
        LLVMStructTypeInContext(context, fields.as_mut_ptr(), fields.len() as u32, 0)
    }
}

/// The LLVM form of `kira_native_bridge::array::KiraArray`.
///
/// `{ usize, usize, ptr, usize }` — the share count last, where that crate's
/// layout test puts it, so the three fields before it keep their offsets.
fn array_header_type(context: LLVMContextRef, usize_ty: LLVMTypeRef) -> LLVMTypeRef {
    // SAFETY: every type is created in this live context; `fields` outlives the
    // struct-type call.
    unsafe {
        let mut fields = [
            usize_ty,
            usize_ty,
            LLVMPointerTypeInContext(context, 0),
            usize_ty,
        ];
        LLVMStructTypeInContext(context, fields.as_mut_ptr(), fields.len() as u32, 0)
    }
}

/// The LLVM form of `kira_native_bridge::runtime::KiraString`.
///
/// `{ ptr, usize, usize }` — the two words of the owned `Box<[u8]>`, then the
/// share count where that crate's layout test puts it.
fn string_box_type(context: LLVMContextRef, usize_ty: LLVMTypeRef) -> LLVMTypeRef {
    // SAFETY: every type is created in this live context; `fields` outlives the
    // struct-type call.
    unsafe {
        let mut fields = [LLVMPointerTypeInContext(context, 0), usize_ty, usize_ty];
        LLVMStructTypeInContext(context, fields.as_mut_ptr(), fields.len() as u32, 0)
    }
}

/// The LLVM form of `kira_runtime_abi::BridgeValue`.
///
/// `{ i8, [7 x i8], i64 }` — the same 16 bytes, with the reserved gap spelled
/// out rather than left to the compiler, so this and the Rust struct cannot
/// disagree about where the payload sits.
fn bridge_value_type(context: LLVMContextRef) -> LLVMTypeRef {
    // SAFETY: every type is created in this live context; `fields` outlives the
    // struct-type call.
    unsafe {
        let i8_ty = LLVMInt8TypeInContext(context);
        let mut fields = [
            i8_ty,
            LLVMArrayType2(i8_ty, 7),
            LLVMInt64TypeInContext(context),
        ];
        LLVMStructTypeInContext(context, fields.as_mut_ptr(), fields.len() as u32, 0)
    }
}
