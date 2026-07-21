//! The LLVM vocabulary one module is built from: the types Kira's values map
//! onto, and the declarations of the runtime helpers its code calls.
//!
//! Everything here is created once per module, in that module's context, before
//! any body is lowered.

use std::ffi::{CStr, CString};

use llvm_sys::core::*;
use llvm_sys::prelude::*;

use super::ffi::c_string;

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
    /// `BridgeValue`: `{ i8 tag, [7 x i8] reserved, i64 payload }`.
    ///
    /// Mirrors `kira_runtime_abi::BridgeValue` exactly; that crate's layout test
    /// is what pins the shape this must agree with.
    pub(super) bridge_value: LLVMTypeRef,
}

impl Types {
    /// Creates every type in `context`.
    pub(super) fn new(context: LLVMContextRef) -> Types {
        // SAFETY: every type below is created in this live context.
        unsafe {
            Types {
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
            }
        }
    }
}

/// The `kira_rt_*` runtime helpers, declared once per module.
///
/// These names are the wire contract with `kira-native-bridge`; they are
/// append-only and must match its `extern "C"` signatures exactly.
#[derive(Clone, Copy)]
pub(crate) struct Runtime {
    pub(super) print_int: Callable,
    pub(super) print_float: Callable,
    pub(super) print_bool: Callable,
    pub(super) print_str: Callable,
    pub(super) str_new: Callable,
    pub(super) str_clone: Callable,
    pub(super) str_concat: Callable,
    pub(super) str_eq: Callable,
    pub(super) str_free: Callable,
    pub(super) array_new: Callable,
    pub(super) array_len: Callable,
    pub(super) array_slot: Callable,
    pub(super) array_push_slot: Callable,
    pub(super) array_clone: Callable,
    pub(super) array_free: Callable,
    pub(super) enum_new: Callable,
    pub(super) enum_tag: Callable,
    /// Reads an enum's payload as an owned word (`match` arm bindings).
    pub(super) enum_payload: Callable,
    pub(super) enum_clone: Callable,
    pub(super) enum_free: Callable,
    /// Allocates the storage one exported class instance lives in when it
    /// crosses to a consumer as a handle.
    pub(super) box_new: Callable,
    /// Releases a box, after the generated drop trampoline has released
    /// whatever the instance inside it owned.
    pub(super) box_free: Callable,
    pub(super) trap_div_zero: Callable,
    /// `kira_rt_trap_foreign`: how a generated adapter's non-success status
    /// becomes a native trap at a foreign call site.
    pub(super) trap_foreign: Callable,
    /// The version marker every emitted program references; see
    /// [`kira_runtime_abi::RUNTIME_ABI_MARKER`].
    pub(super) abi_marker: Callable,
    /// The foreign-adapter version marker every generated adapter references, so
    /// a stale sidecar fails to link by name; see
    /// [`kira_runtime_abi::FOREIGN_ADAPTER_ABI_MARKER`].
    pub(super) foreign_marker: Callable,
    /// `kira_rt_cstring_new`: builds transient NUL-terminated C storage from a
    /// Kira string handle for one foreign call (null on interior NUL).
    pub(super) cstring_new: Callable,
    /// `kira_rt_cstring_free`: frees the storage `kira_rt_cstring_new` produced.
    pub(super) cstring_free: Callable,
    /// `kira_hybrid_call_runtime`: how native code reaches the VM half.
    pub(super) call_runtime: Callable,
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

/// Declares the `kira_rt_*` helpers the lowering calls.
pub(super) fn declare_runtime(module: LLVMModuleRef, types: &Types) -> Runtime {
    // SAFETY: every type belongs to this module's context, and each parameter
    // slice outlives its `LLVMFunctionType` call.
    unsafe {
        let declare = |name: &CStr, ret: LLVMTypeRef, params: &mut [LLVMTypeRef]| -> Callable {
            let ty = LLVMFunctionType(ret, params.as_mut_ptr(), params.len() as u32, 0);
            Callable {
                ty,
                value: LLVMAddFunction(module, name.as_ptr(), ty),
            }
        };
        Runtime {
            print_int: declare(c"kira_rt_print_int", types.void, &mut [types.i64]),
            print_float: declare(c"kira_rt_print_float", types.void, &mut [types.f64]),
            print_bool: declare(c"kira_rt_print_bool", types.void, &mut [types.i8]),
            print_str: declare(c"kira_rt_print_str", types.void, &mut [types.ptr]),
            str_new: declare(c"kira_rt_str_new", types.ptr, &mut [types.ptr, types.i64]),
            str_clone: declare(c"kira_rt_str_clone", types.ptr, &mut [types.ptr]),
            str_concat: declare(
                c"kira_rt_str_concat",
                types.ptr,
                &mut [types.ptr, types.ptr],
            ),
            str_eq: declare(c"kira_rt_str_eq", types.i8, &mut [types.ptr, types.ptr]),
            str_free: declare(c"kira_rt_str_free", types.void, &mut [types.ptr]),
            // The array helpers are generic over the element type: a size and
            // a clone/free callback are all they need, so one declaration each
            // serves every array type. Appended after the string helpers,
            // which is what makes them not an ABI change.
            array_new: declare(c"kira_rt_array_new", types.ptr, &mut [types.i64, types.i64]),
            array_len: declare(c"kira_rt_array_len", types.i64, &mut [types.ptr]),
            array_slot: declare(
                c"kira_rt_array_slot",
                types.ptr,
                &mut [types.ptr, types.i64, types.i64],
            ),
            array_push_slot: declare(
                c"kira_rt_array_push_slot",
                types.ptr,
                &mut [types.ptr, types.i64],
            ),
            array_clone: declare(
                c"kira_rt_array_clone",
                types.ptr,
                &mut [types.ptr, types.i64, types.ptr],
            ),
            array_free: declare(
                c"kira_rt_array_free",
                types.void,
                &mut [types.ptr, types.i64, types.ptr],
            ),
            // The enum helpers box a tag plus a type-erased one-word payload,
            // with a flag saying whether that word is an owned string handle to
            // clone/free. One declaration each serves every enum type. Appended
            // after the array helpers, which is what makes them not an ABI
            // change.
            enum_new: declare(
                c"kira_rt_enum_new",
                types.ptr,
                // (tag, owns_str, payload)
                &mut [types.i64, types.i64, types.i64],
            ),
            enum_tag: declare(c"kira_rt_enum_tag", types.i64, &mut [types.ptr]),
            enum_payload: declare(c"kira_rt_enum_payload", types.i64, &mut [types.ptr]),
            enum_clone: declare(c"kira_rt_enum_clone", types.ptr, &mut [types.ptr]),
            enum_free: declare(c"kira_rt_enum_free", types.void, &mut [types.ptr]),
            // The box helpers hold an exported class instance for as long as a
            // consumer holds its handle. Appended after the enum helpers, which
            // is what makes them not an ABI change.
            box_new: declare(c"kira_rt_box_new", types.ptr, &mut [types.i64]),
            box_free: declare(c"kira_rt_box_free", types.void, &mut [types.ptr, types.i64]),
            trap_div_zero: declare(c"kira_rt_trap_div_zero", types.void, &mut []),
            trap_foreign: declare(c"kira_rt_trap_foreign", types.void, &mut [types.i32]),
            abi_marker: declare(&abi_marker_symbol(), types.void, &mut []),
            // Appended after the runtime marker; the foreign helpers are an
            // append-only addition to the `kira_rt_*` surface. An adapter
            // references the marker so a stale sidecar fails to link by name.
            foreign_marker: declare(&foreign_marker_symbol(), types.void, &mut []),
            cstring_new: declare(c"kira_rt_cstring_new", types.ptr, &mut [types.ptr]),
            cstring_free: declare(c"kira_rt_cstring_free", types.void, &mut [types.ptr]),
            call_runtime: declare(
                c"kira_hybrid_call_runtime",
                types.void,
                // (function_id, args, count, out)
                &mut [types.i32, types.ptr, types.i32, types.ptr],
            ),
        }
    }
}

/// The runtime ABI marker's symbol, as a C string.
///
/// Built from the shared constant rather than spelled here, so the backend and
/// the runtime archive cannot drift apart silently.
fn abi_marker_symbol() -> CString {
    c_string(kira_runtime_abi::RUNTIME_ABI_MARKER)
}

/// The foreign-adapter ABI marker's symbol, as a C string.
///
/// Built from the shared constant so the backend and the native bridge that
/// defines it cannot drift apart silently.
fn foreign_marker_symbol() -> CString {
    c_string(kira_runtime_abi::FOREIGN_ADAPTER_ABI_MARKER)
}
