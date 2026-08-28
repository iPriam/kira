//! Runtime helper declarations for one LLVM module.

use std::ffi::{CStr, CString};

use llvm_sys::core::*;
use llvm_sys::prelude::*;

use kira_runtime_abi::{CompilerOp, EnvOp, FileSystemOp};

use super::super::ffi::c_string;
use super::runtime::Runtime;
use super::{Callable, Types};

/// Declares the `kira_rt_*` helpers the lowering calls.
pub(in crate::codegen) fn declare_runtime(module: LLVMModuleRef, types: &Types) -> Runtime {
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
            task_reset: declare(c"kira_rt_task_reset", types.void, &mut []),
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
            cstring_new: declare(c"kira_rt_cstring_new", types.ptr, &mut [types.ptr]),
            cstring_free: declare(c"kira_rt_cstring_free", types.void, &mut [types.ptr]),
            ffi_call: declare(
                c"kira_rt_ffi_call_bytes",
                types.i32,
                &mut [types.ptr, types.ptr, types.ptr, types.ptr],
            ),
            ffi_closure: declare(
                c"kira_rt_ffi_closure",
                types.i64,
                &mut [types.ptr, types.ptr],
            ),
            cblock_text: declare(c"kira_rt_cblock_text", types.i64, &mut [types.ptr]),
            cblock_bytes: declare(
                c"kira_rt_cblock_bytes",
                types.i64,
                &mut [types.ptr, types.i64],
            ),
            cblock_alien: declare(c"kira_rt_cblock_alien", types.i64, &mut [types.i64]),
            cblock_word: declare(c"kira_rt_cblock_word", types.i64, &mut [types.i64]),
            cblock_clone: declare(c"kira_rt_cblock_clone", types.i64, &mut [types.i64]),
            cblock_free: declare(c"kira_rt_cblock_free", types.void, &mut [types.i64]),
            cblock_attach: declare(
                c"kira_rt_cblock_attach",
                types.void,
                &mut [types.i64, types.i64, types.i64, types.i64],
            ),
            cblock_keep: declare(c"kira_rt_cblock_keep", types.void, &mut [types.i64]),
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
                declare(
                    c"kira_rt_string_drop_last_scalar",
                    types.ptr,
                    &mut [types.ptr],
                ),
                declare(c"kira_rt_string_is_int", types.i1, &mut [types.ptr]),
                declare(c"kira_rt_string_to_int", types.i64, &mut [types.ptr]),
            ],
            scalar_text: declare(c"kira_rt_scalar_text", types.ptr, &mut [types.i64]),
            array_elements: declare(
                c"kira_rt_array_elements",
                types.i64,
                &mut [types.ptr, types.i32, types.i64],
            ),
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
            native_value_any: declare(
                c"kira_rt_native_value_any",
                types.ptr,
                &mut [types.i64, types.ptr],
            ),
            native_value_read_any_type: declare(
                c"kira_rt_native_value_read_any_type",
                types.i64,
                &mut [types.ptr],
            ),
            native_value_raw_ptr: declare(
                c"kira_rt_native_value_raw_ptr",
                types.ptr,
                &mut [types.i64],
            ),
            native_value_cell: declare(c"kira_rt_native_value_cell", types.ptr, &mut [types.ptr]),
            native_value_read_cell: declare(
                c"kira_rt_native_value_read_cell",
                types.ptr,
                &mut [types.ptr],
            ),
            native_value_float: declare(c"kira_rt_native_value_float", types.ptr, &mut [types.f64]),
            native_value_bool: declare(c"kira_rt_native_value_bool", types.ptr, &mut [types.i8]),
            native_value_string: declare(
                c"kira_rt_native_value_string",
                types.ptr,
                &mut [types.ptr],
            ),
            native_value_cblock_from_handle: declare(
                c"kira_rt_native_value_cblock_from_handle",
                types.ptr,
                &mut [types.i64],
            ),
            native_value_cblock_to_handle: declare(
                c"kira_rt_native_value_cblock_to_handle",
                types.i64,
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
            // Appended after the compiler helpers. The environment operations
            // have a few fixed signatures, kept in one table so the native
            // bridge and the lowering cannot drift apart.
            env: EnvOp::ALL.map(|op| {
                let name = c_string(op.runtime_symbol());
                let (ret, mut params) = match op {
                    EnvOp::Text => (types.ptr, vec![types.ptr]),
                    EnvOp::IsSet => (types.i8, vec![types.ptr]),
                    EnvOp::ArgumentCount => (types.i64, vec![]),
                    EnvOp::Argument => (types.ptr, vec![types.i64]),
                    EnvOp::Sleep => (types.void, vec![types.i64]),
                };
                declare(&name, ret, &mut params)
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
