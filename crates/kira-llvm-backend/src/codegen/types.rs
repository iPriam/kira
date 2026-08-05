//! The LLVM vocabulary one module is built from: the types Kira's values map
//! onto, and the declarations of the runtime helpers its code calls.
//!
//! Everything here is created once per module, in that module's context, before
//! any body is lowered.

use std::ffi::{CStr, CString};

use llvm_sys::core::*;
use llvm_sys::prelude::*;

use kira_runtime_abi::{CompilerOp, EnvOp, FileSystemOp, ForeignPointerWidth};

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
    /// The **target**'s `usize`, for the runtime helpers that take one.
    ///
    /// Not this machine's: a wasm32 module links against a `kira-native-bridge`
    /// compiled for wasm32, where `usize` is 32 bits. Declaring it as `i64`
    /// there is a signature the linker resolves by name and the module traps
    /// on, which is exactly what happened to every wasm string before this
    /// field existed.
    pub(super) usize_ty: LLVMTypeRef,
    /// `BridgeValue`: `{ i8 tag, [7 x i8] reserved, i64 payload }`.
    ///
    /// Mirrors `kira_runtime_abi::BridgeValue` exactly; that crate's layout test
    /// is what pins the shape this must agree with.
    pub(super) bridge_value: LLVMTypeRef,
    /// `KiraEnum`: `{ i64 tag, i64 payload_kind, i64 payload, usize shares }`.
    ///
    /// The one runtime box this module reaches into rather than calling a helper
    /// for. Copying and releasing an enum is a share count away from free —
    /// see [`super::values`] — and at four hundred thousand of each per frame,
    /// the *call* was the cost. Mirrors `kira_native_bridge::enums::KiraEnum`,
    /// whose layout test in that crate pins what this must agree with, and
    /// whose version is the one `RUNTIME_ABI_MARKER` names.
    pub(super) enum_box: LLVMTypeRef,
    /// `KiraArray`: `{ usize len, usize cap, ptr items, usize shares }`.
    ///
    /// Read for the same reason [`Types::enum_box`] is: copying an array is a
    /// share count away from free, and generated code does it often enough that
    /// the call was the cost. Mirrors `kira_native_bridge::array::KiraArray`,
    /// whose layout test pins what this must agree with.
    pub(super) array_header: LLVMTypeRef,
    /// `KiraString`: `{ ptr, usize len, usize shares }`.
    ///
    /// The `Box<[u8]>` a string owns is a fat pointer, so the count sits after
    /// two words. Read for the same reason the other two are: a string is never
    /// written after it is built, so copying one is a count away from free.
    /// Mirrors `kira_native_bridge::runtime::KiraString`, whose layout test
    /// pins what this must agree with.
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
    pub(super) str_concat: Callable,
    pub(super) str_eq: Callable,
    pub(super) str_free: Callable,
    pub(super) array_new: Callable,
    pub(super) array_len: Callable,
    pub(super) array_slot: Callable,
    /// The address of an element to write, in a block the handle owns alone.
    pub(super) array_slot_mut: Callable,
    pub(super) array_push_slot: Callable,
    /// Releases the *last* hold on an array: the only release that frees its
    /// elements, and the only one generated code still calls. A copy and every
    /// earlier release are emitted inline; see `super::values`.
    pub(super) array_free: Callable,
    pub(super) enum_new: Callable,
    /// Structural equality of two erased values, and of the enums inside them.
    pub(super) any_eq: Callable,
    /// Structural equality of two arrays, given the element's equality leaf.
    pub(super) array_eq: Callable,
    /// Boxes a moved aggregate payload with clone/free **and** equality leaves.
    pub(super) enum_new_aggregate_eq: Callable,
    /// Boxes a moved struct payload with type-specific clone/free leaves.
    pub(super) enum_new_aggregate: Callable,
    pub(super) enum_tag: Callable,
    /// Reads an enum's payload as an owned word (`match` arm bindings).
    pub(super) enum_payload: Callable,
    /// Reads an aggregate payload into caller-owned storage.
    pub(super) enum_payload_aggregate: Callable,
    /// Releases the *last* hold on an enum box: the only release that touches
    /// the payload, and the only one generated code still calls. A copy and
    /// every earlier release are emitted inline; see `super::values`.
    pub(super) enum_free: Callable,
    /// Boxes a value into a fresh capture cell.
    pub(super) cell_new: Callable,
    /// Boxes a wide value — a struct or an array handle — into a fresh cell.
    pub(super) cell_new_aggregate: Callable,
    /// Reads what a cell holds as an owned word.
    pub(super) cell_get: Callable,
    /// Reads a wide payload into caller-owned storage.
    pub(super) cell_get_aggregate: Callable,
    /// Replaces what a cell holds, releasing the old payload, in one call.
    pub(super) cell_set: Callable,
    /// Replaces a cell's payload with a wide value.
    pub(super) cell_set_aggregate: Callable,
    /// Releases the *last* hold on a cell: the only release that touches the
    /// payload, and the only one generated code still calls. A copy and every
    /// earlier release are emitted inline; see `super::values`.
    pub(super) cell_free: Callable,
    /// Allocates the storage one exported class instance lives in when it
    /// crosses to a consumer as a handle.
    pub(super) box_new: Callable,
    /// Releases a box, after the generated drop trampoline has released
    /// whatever the instance inside it owned.
    pub(super) box_free: Callable,
    pub(super) trap_div_zero: Callable,
    /// `kira_rt_task_op`: the whole deferred-task surface, in one call.
    ///
    /// One symbol rather than a dozen, because the *policy* is generated Kira
    /// the IR synthesizes: what the runtime owns is the task table, and one
    /// `(primitive, a, b, c) -> answer` shape covers every question the
    /// generated scheduler asks it.
    pub(super) task_op: Callable,
    /// `kira_rt_trap_foreign`: how a generated adapter's non-success status
    /// becomes a native trap at a foreign call site.
    pub(super) trap_foreign: Callable,
    /// `kira_rt_trap_foreign_array`: a Kira array too long for the inline C
    /// array a `@FFI.Array` member reserves.
    pub(super) trap_foreign_array: Callable,
    /// `kira_rt_trap_foreign_unavailable`: a call into a native library this
    /// platform does not have, named rather than numbered.
    pub(super) trap_foreign_unavailable: Callable,
    /// `llvm.stacksave` / `llvm.stackrestore`: the pair that gives back stack an
    /// `alloca` took, so a call-site reservation lasts only as long as the call.
    pub(super) stack_save: Callable,
    pub(super) stack_restore: Callable,
    /// The version marker every emitted program references; see
    /// [`kira_runtime_abi::RUNTIME_ABI_MARKER`].
    pub(super) abi_marker: Callable,
    /// Reports the native heap balance at exit; silent unless asked.
    pub(super) heap_report: Callable,
    /// The foreign-adapter version marker every generated adapter references, so
    /// a stale sidecar fails to link by name; see
    /// [`kira_runtime_abi::FOREIGN_ADAPTER_ABI_MARKER`].
    pub(super) foreign_marker: Callable,
    /// `kira_rt_str_from_cstr`: copies a `CString` result's bytes out of the
    /// storage the callee keeps, which is how a returned C string becomes an
    /// owned Kira `String` with nothing to free on this side.
    pub(super) str_from_cstr: Callable,
    /// `kira_rt_str_count`: a string's length in bytes, consuming it.
    pub(super) str_count: Callable,
    /// `kira_rt_str_char_at`: the byte at an index, consuming the string.
    pub(super) str_char_at: Callable,
    /// `kira_rt_str_substring`: a half-open byte slice as a fresh string,
    /// consuming the original.
    pub(super) str_substring: Callable,
    /// `kira_rt_str_index_of`: the first byte index of a needle, or `-1`,
    /// consuming both strings.
    pub(super) str_index_of: Callable,
    /// The shared-opcode string operations, one callable each, in
    /// [`StringOp`](kira_runtime_abi::StringOp) order. Indexed by the operand
    /// byte rather than matched by name, so a new operation is a row here and
    /// nothing in the lowering.
    pub(super) string_ops: [Callable; kira_runtime_abi::StringOp::ALL.len()],
    /// `kira_rt_str_of_int` / `_float` / `_bool`: a scalar rendered as a fresh
    /// string, in exactly the spelling `print` gives it.
    pub(super) str_of_int: Callable,
    /// See [`RuntimeCallables::str_of_int`].
    pub(super) str_of_float: Callable,
    /// See [`RuntimeCallables::str_of_int`].
    pub(super) str_of_bool: Callable,
    /// `kira_rt_cstring_new`: builds transient NUL-terminated C storage from a
    /// Kira string handle for one foreign call (null on interior NUL).
    pub(super) cstring_new: Callable,
    /// `kira_rt_cstring_free`: frees the storage `kira_rt_cstring_new` produced.
    pub(super) cstring_free: Callable,
    /// `kira_rt_cstring_retain`: C storage for a `CString` struct member, never
    /// freed because C keeps reading it after the call returns.
    pub(super) cstring_retain: Callable,
    /// `kira_rt_clayout_retain`: the same storage rule for a struct handed to C
    /// by address.
    pub(super) clayout_retain: Callable,
    /// `kira_hybrid_call_runtime`: how native code reaches the VM half.
    pub(super) call_runtime: Callable,
    pub(super) native_value_int: Callable,
    pub(super) native_value_raw_ptr: Callable,
    pub(super) native_value_float: Callable,
    pub(super) native_value_bool: Callable,
    pub(super) native_value_string: Callable,
    pub(super) native_value_aggregate: Callable,
    pub(super) native_value_set_child: Callable,
    pub(super) native_value_read_int: Callable,
    pub(super) native_value_read_raw_ptr: Callable,
    pub(super) native_value_read_float: Callable,
    pub(super) native_value_read_bool: Callable,
    pub(super) native_value_read_string: Callable,
    pub(super) native_value_enum_tag: Callable,
    pub(super) native_value_child: Callable,
    pub(super) native_value_free: Callable,
    pub(super) native_value_array_from: Callable,
    pub(super) native_value_array_to: Callable,
    pub(super) native_state_new: Callable,
    pub(super) native_state_recover: Callable,
    pub(super) native_state_replace: Callable,
    pub(super) native_state_free: Callable,
    /// Allocates a box holding one state value in this backend's own layout.
    pub(super) native_state_box_new: Callable,
    /// The address of the value inside a box, type-checked.
    pub(super) native_state_box_payload: Callable,
    pub(super) trap_native_state: Callable,
    /// The `kira_rt_fs_*` helpers, indexed by [`FileSystemOp::as_byte`].
    ///
    /// An array rather than twelve fields, and indexed by a total function
    /// rather than searched: adding an operation adds a row to
    /// [`FileSystemOp::ALL`] and nothing here has to be remembered.
    pub(super) file_system: [Callable; FileSystemOp::ALL.len()],
    /// The `kira_rt_compiler_*` helpers, indexed by [`CompilerOp::as_byte`].
    ///
    /// An array for the same reason the file-system row is: the set is written
    /// down once, in [`CompilerOp::ALL`], and indexed by a total function.
    pub(super) compiler: [Callable; CompilerOp::ALL.len()],
    /// The `kira_rt_env_*` helpers, indexed by [`EnvOp::as_byte`].
    pub(super) env: [Callable; EnvOp::ALL.len()],
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
            str_new: declare(
                c"kira_rt_str_new",
                types.ptr,
                &mut [types.ptr, types.usize_ty],
            ),
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
            // The two mutating entry points take the element clone as well: a
            // write is where a shared item block becomes this handle's own, and
            // the leaf is what makes that copy deep. See `kira-native-bridge`'s
            // `array` module.
            array_slot_mut: declare(
                c"kira_rt_array_slot_mut",
                types.ptr,
                &mut [types.ptr, types.i64, types.i64, types.ptr],
            ),
            array_push_slot: declare(
                c"kira_rt_array_push_slot",
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
                // (tag, payload_kind, payload)
                &mut [types.i64, types.i64, types.i64],
            ),
            any_eq: declare(c"kira_rt_any_eq", types.i8, &mut [types.ptr, types.ptr]),
            array_eq: declare(
                c"kira_rt_array_eq",
                types.i8,
                // (a, b, element size, element equality leaf)
                &mut [types.ptr, types.ptr, types.i64, types.ptr],
            ),
            enum_new_aggregate_eq: declare(
                c"kira_rt_enum_new_aggregate_eq",
                types.ptr,
                // (tag, source, size, clone, free, eq)
                &mut [
                    types.i64, types.ptr, types.i64, types.ptr, types.ptr, types.ptr,
                ],
            ),
            enum_new_aggregate: declare(
                c"kira_rt_enum_new_aggregate",
                types.ptr,
                // (tag, source, size, clone, free)
                &mut [types.i64, types.ptr, types.i64, types.ptr, types.ptr],
            ),
            enum_tag: declare(c"kira_rt_enum_tag", types.i64, &mut [types.ptr]),
            enum_payload: declare(c"kira_rt_enum_payload", types.i64, &mut [types.ptr]),
            enum_payload_aggregate: declare(
                c"kira_rt_enum_payload_aggregate",
                types.void,
                &mut [types.ptr, types.ptr],
            ),
            enum_free: declare(c"kira_rt_enum_free", types.void, &mut [types.ptr]),
            // The capture-cell helpers box the shared, mutable storage a
            // captured `var` lives in. The box is an enum box with the tag
            // unused, so `Types::enum_box` serves the inline share bump and
            // only the *write* needs machinery of its own. Appended after the
            // enum helpers, which is what makes them not an ABI change.
            cell_new: declare(
                c"kira_rt_cell_new",
                types.ptr,
                // (payload_kind, payload)
                &mut [types.i64, types.i64],
            ),
            cell_new_aggregate: declare(
                c"kira_rt_cell_new_aggregate",
                types.ptr,
                // (source, size, clone, free)
                &mut [types.ptr, types.i64, types.ptr, types.ptr],
            ),
            cell_get: declare(c"kira_rt_cell_get", types.i64, &mut [types.ptr]),
            cell_get_aggregate: declare(
                c"kira_rt_cell_get_aggregate",
                types.void,
                &mut [types.ptr, types.ptr],
            ),
            cell_set: declare(
                c"kira_rt_cell_set",
                types.void,
                // (cell, payload_kind, payload)
                &mut [types.ptr, types.i64, types.i64],
            ),
            cell_set_aggregate: declare(
                c"kira_rt_cell_set_aggregate",
                types.void,
                // (cell, source, size, clone, free)
                &mut [types.ptr, types.ptr, types.i64, types.ptr, types.ptr],
            ),
            cell_free: declare(c"kira_rt_cell_free", types.void, &mut [types.ptr]),
            // The box helpers hold an exported class instance for as long as a
            // consumer holds its handle. Appended after the enum helpers, which
            // is what makes them not an ABI change.
            box_new: declare(c"kira_rt_box_new", types.ptr, &mut [types.i64]),
            box_free: declare(c"kira_rt_box_free", types.void, &mut [types.ptr, types.i64]),
            trap_div_zero: declare(c"kira_rt_trap_div_zero", types.void, &mut []),
            task_op: declare(
                c"kira_rt_task_op",
                types.i64,
                &mut [types.i64, types.i64, types.i64, types.i64],
            ),
            stack_save: declare(c"llvm.stacksave.p0", types.ptr, &mut []),
            stack_restore: declare(c"llvm.stackrestore.p0", types.void, &mut [types.ptr]),
            trap_foreign_unavailable: declare(
                c"kira_rt_trap_foreign_unavailable",
                types.void,
                &mut [types.ptr, types.ptr],
            ),
            trap_foreign: declare(c"kira_rt_trap_foreign", types.void, &mut [types.i32]),
            trap_foreign_array: declare(
                c"kira_rt_trap_foreign_array",
                types.void,
                &mut [types.i64, types.i64],
            ),
            abi_marker: declare(&abi_marker_symbol(), types.void, &mut []),
            heap_report: declare(c"kira_rt_heap_report", types.void, &mut []),
            // Appended after the runtime marker; the foreign helpers are an
            // append-only addition to the `kira_rt_*` surface. An adapter
            // references the marker so a stale sidecar fails to link by name.
            foreign_marker: declare(&foreign_marker_symbol(), types.void, &mut []),
            cstring_new: declare(c"kira_rt_cstring_new", types.ptr, &mut [types.ptr]),
            cstring_free: declare(c"kira_rt_cstring_free", types.void, &mut [types.ptr]),
            cstring_retain: declare(c"kira_rt_cstring_retain", types.i64, &mut [types.ptr]),
            clayout_retain: declare(
                c"kira_rt_clayout_retain",
                types.i64,
                &mut [types.ptr, types.i64],
            ),
            str_from_cstr: declare(c"kira_rt_str_from_cstr", types.ptr, &mut [types.ptr]),
            str_count: declare(c"kira_rt_str_count", types.i64, &mut [types.ptr]),
            str_char_at: declare(
                c"kira_rt_str_char_at",
                types.i64,
                &mut [types.ptr, types.i64],
            ),
            str_substring: declare(
                c"kira_rt_str_substring",
                types.ptr,
                &mut [types.ptr, types.i64, types.i64],
            ),
            str_index_of: declare(
                c"kira_rt_str_index_of",
                types.i64,
                &mut [types.ptr, types.ptr],
            ),
            // One per `StringOp`, in wire order. The three predicates answer
            // `i1`, `split` answers an array pointer and the rest a string
            // pointer; every argument is a string handle.
            string_ops: [
                declare(
                    c"kira_rt_string_contains",
                    types.i1,
                    &mut [types.ptr, types.ptr],
                ),
                declare(
                    c"kira_rt_string_starts_with",
                    types.i1,
                    &mut [types.ptr, types.ptr],
                ),
                declare(
                    c"kira_rt_string_ends_with",
                    types.i1,
                    &mut [types.ptr, types.ptr],
                ),
                declare(
                    c"kira_rt_string_split",
                    types.ptr,
                    &mut [types.ptr, types.ptr],
                ),
                declare(
                    c"kira_rt_string_replace",
                    types.ptr,
                    &mut [types.ptr, types.ptr, types.ptr],
                ),
                declare(c"kira_rt_string_trim", types.ptr, &mut [types.ptr]),
                declare(c"kira_rt_string_lowercase", types.ptr, &mut [types.ptr]),
                declare(c"kira_rt_string_uppercase", types.ptr, &mut [types.ptr]),
            ],
            str_of_int: declare(c"kira_rt_str_of_int", types.ptr, &mut [types.i64]),
            str_of_float: declare(c"kira_rt_str_of_float", types.ptr, &mut [types.f64]),
            str_of_bool: declare(c"kira_rt_str_of_bool", types.ptr, &mut [types.i1]),
            call_runtime: declare(
                c"kira_hybrid_call_runtime",
                types.void,
                // (function_id, args, count, out)
                &mut [types.i32, types.ptr, types.i32, types.ptr],
            ),
            native_value_int: declare(c"kira_rt_native_value_int", types.ptr, &mut [types.i64]),
            native_value_raw_ptr: declare(
                c"kira_rt_native_value_raw_ptr",
                types.ptr,
                &mut [types.i64],
            ),
            native_value_float: declare(c"kira_rt_native_value_float", types.ptr, &mut [types.f64]),
            native_value_bool: declare(c"kira_rt_native_value_bool", types.ptr, &mut [types.i8]),
            native_value_string: declare(
                c"kira_rt_native_value_string",
                types.ptr,
                &mut [types.ptr],
            ),
            native_value_aggregate: declare(
                c"kira_rt_native_value_aggregate",
                types.ptr,
                &mut [types.i32, types.i32, types.i64],
            ),
            native_value_set_child: declare(
                c"kira_rt_native_value_set_child",
                types.i32,
                &mut [types.ptr, types.i64, types.ptr],
            ),
            native_value_read_int: declare(
                c"kira_rt_native_value_read_int",
                types.i64,
                &mut [types.ptr],
            ),
            native_value_read_raw_ptr: declare(
                c"kira_rt_native_value_read_raw_ptr",
                types.i64,
                &mut [types.ptr],
            ),
            native_value_read_float: declare(
                c"kira_rt_native_value_read_float",
                types.f64,
                &mut [types.ptr],
            ),
            native_value_read_bool: declare(
                c"kira_rt_native_value_read_bool",
                types.i8,
                &mut [types.ptr],
            ),
            native_value_read_string: declare(
                c"kira_rt_native_value_read_string",
                types.ptr,
                &mut [types.ptr],
            ),
            native_value_enum_tag: declare(
                c"kira_rt_native_value_enum_tag",
                types.i32,
                &mut [types.ptr],
            ),
            native_value_child: declare(
                c"kira_rt_native_value_child",
                types.ptr,
                &mut [types.ptr, types.i64],
            ),
            native_value_free: declare(c"kira_rt_native_value_free", types.void, &mut [types.ptr]),
            native_value_array_from: declare(
                c"kira_rt_native_value_array_from",
                types.ptr,
                // (array, esize, element clone, encode) — encoding moves every
                // element out of the block, so it takes the same leaf a write
                // does to make the block the array's own first.
                &mut [types.ptr, types.i64, types.ptr, types.ptr],
            ),
            native_value_array_to: declare(
                c"kira_rt_native_value_array_to",
                types.ptr,
                &mut [types.ptr, types.i64, types.ptr],
            ),
            native_state_new: declare(
                c"kira_rt_native_state_new",
                types.i32,
                &mut [types.i64, types.ptr, types.ptr],
            ),
            native_state_recover: declare(
                c"kira_rt_native_state_recover",
                types.i32,
                &mut [types.i64, types.i64, types.ptr],
            ),
            native_state_replace: declare(
                c"kira_rt_native_state_replace",
                types.i32,
                &mut [types.i64, types.i64, types.ptr],
            ),
            native_state_free: declare(c"kira_rt_native_state_free", types.i32, &mut [types.i64]),
            native_state_box_new: declare(
                c"kira_rt_native_state_box_new",
                types.i32,
                &mut [types.i64, types.i64, types.i64, types.ptr, types.ptr],
            ),
            native_state_box_payload: declare(
                c"kira_rt_native_state_box_payload",
                types.i32,
                &mut [types.i64, types.i64, types.ptr],
            ),
            trap_native_state: declare(c"kira_rt_trap_native_state", types.void, &mut [types.i32]),
            // Appended after the callback-state helpers. Each row's shape comes
            // from the operation itself, so the twelve declarations are one
            // table walk rather than twelve hand-written lines that could
            // disagree with the runtime.
            file_system: FileSystemOp::ALL.map(|op| {
                let name = c_string(op.runtime_symbol());
                let (ret, mut params) = file_system_signature(op, types);
                declare(&name, ret, &mut params)
            }),
            // Appended after the file-system helpers. Every compiler operation
            // takes one `[String]` handle plus the element stride and answers
            // with another, so the shape is one row rather than a table.
            compiler: CompilerOp::ALL.map(|op| {
                let name = c_string(op.runtime_symbol());
                declare(&name, types.ptr, &mut [types.ptr, types.i64])
            }),
            // Appended after the compiler helpers. Each takes one string
            // handle; `Text` answers with another and `IsSet` with a flag, so
            // the return type is the one thing that differs between them.
            env: EnvOp::ALL.map(|op| {
                let name = c_string(op.runtime_symbol());
                let ret = match op {
                    EnvOp::Text => types.ptr,
                    EnvOp::IsSet => types.i8,
                };
                declare(&name, ret, &mut [types.ptr])
            }),
        }
    }
}

/// The C-ABI shape of one `kira_rt_fs_*` helper.
///
/// A path is a string handle (`ptr`), a flag comes back as an `i8`, a size as an
/// `i64`, and an operation that builds an array takes the element stride the
/// backend computed for the target and returns the array's handle.
fn file_system_signature(op: FileSystemOp, types: &Types) -> (LLVMTypeRef, Vec<LLVMTypeRef>) {
    match op {
        FileSystemOp::ReadRange => (types.ptr, vec![types.ptr, types.i64, types.i64, types.i64]),
        FileSystemOp::WriteBytes => (types.i8, vec![types.ptr, types.ptr, types.i64]),
        FileSystemOp::ReadText => (types.ptr, vec![types.ptr]),
        FileSystemOp::WriteText | FileSystemOp::RenamePath => {
            (types.i8, vec![types.ptr, types.ptr])
        }
        FileSystemOp::ListDirectory => (types.ptr, vec![types.ptr, types.i64]),
        FileSystemOp::FileSize => (types.i64, vec![types.ptr]),
        FileSystemOp::IsDirectory
        | FileSystemOp::MakeDirectory
        | FileSystemOp::RemovePath
        | FileSystemOp::FileExists
        | FileSystemOp::PathExists => (types.i8, vec![types.ptr]),
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
