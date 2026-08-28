//! Runtime helper declarations shared by the LLVM codegen modules.

use super::Callable;
use kira_runtime_abi::{CompilerOp, EnvOp, FileSystemOp};

/// The `kira_rt_*` runtime helpers, declared once per module.
///
/// These names are the wire contract with `kira-native-bridge`; they are
/// append-only and must match its `extern "C"` signatures exactly.
#[derive(Clone, Copy)]
pub(in crate::codegen) struct Runtime {
    pub(in crate::codegen) print_int: Callable,
    pub(in crate::codegen) print_float: Callable,
    pub(in crate::codegen) print_bool: Callable,
    pub(in crate::codegen) print_str: Callable,
    pub(in crate::codegen) str_new: Callable,
    pub(in crate::codegen) str_concat: Callable,
    pub(in crate::codegen) str_eq: Callable,
    pub(in crate::codegen) str_free: Callable,
    pub(in crate::codegen) array_new: Callable,
    pub(in crate::codegen) array_len: Callable,
    pub(in crate::codegen) array_slot: Callable,
    /// The address of an element to write, in a block the handle owns alone.
    pub(in crate::codegen) array_slot_mut: Callable,
    pub(in crate::codegen) array_push_slot: Callable,
    /// Releases the *last* hold on an array: the only release that frees its
    /// elements, and the only one generated code still calls. A copy and every
    /// earlier release are emitted inline; see `super::values`.
    pub(in crate::codegen) array_free: Callable,
    pub(in crate::codegen) enum_new: Callable,
    /// Structural equality of two erased values, and of the enums inside them.
    pub(in crate::codegen) any_eq: Callable,
    /// Structural equality of two arrays, given the element's equality leaf.
    pub(in crate::codegen) array_eq: Callable,
    /// Boxes a moved aggregate payload with clone/free **and** equality leaves.
    pub(in crate::codegen) enum_new_aggregate_eq: Callable,
    /// Boxes a moved aggregate payload with type-specific clone/free leaves.
    pub(in crate::codegen) enum_new_aggregate: Callable,
    pub(in crate::codegen) enum_tag: Callable,
    /// Reads an enum's payload as an owned word (`match` arm bindings).
    pub(in crate::codegen) enum_payload: Callable,
    /// Reads an aggregate payload into caller-owned storage.
    pub(in crate::codegen) enum_payload_aggregate: Callable,
    /// Releases the *last* hold on an enum box: the only release that touches
    /// the payload, and the only one generated code still calls. A copy and
    /// every earlier release are emitted inline; see `super::values`.
    pub(in crate::codegen) enum_free: Callable,
    /// Boxes a value into a fresh capture cell.
    pub(in crate::codegen) cell_new: Callable,
    /// Boxes a wide value — a struct or an array handle — into a fresh cell.
    pub(in crate::codegen) cell_new_aggregate: Callable,
    /// Reads what a cell holds as an owned word.
    pub(in crate::codegen) cell_get: Callable,
    /// Reads a wide payload into caller-owned storage.
    pub(in crate::codegen) cell_get_aggregate: Callable,
    /// Replaces what a cell holds, releasing the old payload, in one call.
    pub(in crate::codegen) cell_set: Callable,
    /// Replaces a cell's payload with a wide value.
    pub(in crate::codegen) cell_set_aggregate: Callable,
    /// Releases the *last* hold on a cell: the only release that touches the
    /// payload, and the only one generated code still calls. A copy and every
    /// earlier release are emitted inline; see `super::values`.
    pub(in crate::codegen) cell_free: Callable,
    /// Allocates the storage one exported class instance lives in when it
    /// crosses to a consumer as a handle.
    pub(in crate::codegen) box_new: Callable,
    /// Releases a box, after the generated drop trampoline has released
    /// whatever the instance inside it owned.
    pub(in crate::codegen) box_free: Callable,
    pub(in crate::codegen) trap_div_zero: Callable,
    /// `kira_rt_task_op`: the whole deferred-task surface, in one call.
    ///
    /// One symbol rather than a dozen, because the *policy* is generated Kira
    /// the IR synthesizes: what the runtime owns is the task table, and one
    /// `(primitive, a, b, c) -> answer` shape covers every question the
    /// generated scheduler asks it.
    pub(in crate::codegen) task_op: Callable,
    /// Resets the native task table at a process or hybrid run boundary.
    pub(in crate::codegen) task_reset: Callable,
    /// `kira_rt_trap_foreign`: how a generated adapter's non-success status
    /// becomes a native trap at a foreign call site.
    pub(in crate::codegen) trap_foreign: Callable,
    /// `kira_rt_trap_foreign_array`: a Kira array too long for the inline C
    /// array a `@FFI.Array` member reserves.
    pub(in crate::codegen) trap_foreign_array: Callable,
    /// `kira_rt_trap_foreign_unavailable`: a call into a native library this
    /// platform does not have, named rather than numbered.
    pub(in crate::codegen) trap_foreign_unavailable: Callable,
    /// `llvm.stacksave` / `llvm.stackrestore`: the pair that gives back stack an
    /// `alloca` took, so a call-site reservation lasts only as long as the call.
    pub(in crate::codegen) stack_save: Callable,
    pub(in crate::codegen) stack_restore: Callable,
    /// The version marker every emitted program references; see
    /// [`kira_runtime_abi::RUNTIME_ABI_MARKER`].
    pub(in crate::codegen) abi_marker: Callable,
    /// Reports the native heap balance at exit; silent unless asked.
    pub(in crate::codegen) heap_report: Callable,
    /// `kira_rt_str_from_cstr`: copies a `CString` result's bytes out of the
    /// storage the callee keeps, which is how a returned C string becomes an
    /// owned Kira `String` with nothing to free on this side.
    pub(in crate::codegen) str_from_cstr: Callable,
    /// `kira_rt_str_count`: a string's length in bytes, consuming it.
    pub(in crate::codegen) str_count: Callable,
    /// `kira_rt_str_char_at`: the byte at an index, consuming the string.
    pub(in crate::codegen) str_char_at: Callable,
    /// `kira_rt_str_substring`: a half-open byte slice as a fresh string,
    /// consuming the original.
    pub(in crate::codegen) str_substring: Callable,
    /// `kira_rt_str_index_of`: the first byte index of a needle, or `-1`,
    /// consuming both strings.
    pub(in crate::codegen) str_index_of: Callable,
    /// The shared-opcode string operations, one callable each, in
    /// [`StringOp`](kira_runtime_abi::StringOp) order. Indexed by the operand
    /// byte rather than matched by name, so a new operation is a row here and
    /// nothing in the lowering.
    pub(in crate::codegen) string_ops: [Callable; kira_runtime_abi::StringOp::ALL.len()],
    /// `kira_rt_scalar_text`: one Unicode scalar's text, from its code point.
    pub(in crate::codegen) scalar_text: Callable,
    /// `kira_rt_array_elements`: an array's elements written out in C's widths.
    pub(in crate::codegen) array_elements: Callable,
    /// `kira_rt_str_of_int` / `_float` / `_bool`: a scalar rendered as a fresh
    /// string, in exactly the spelling `print` gives it.
    pub(in crate::codegen) str_of_int: Callable,
    /// See [`RuntimeCallables::str_of_int`].
    pub(in crate::codegen) str_of_float: Callable,
    /// See [`RuntimeCallables::str_of_int`].
    pub(in crate::codegen) str_of_bool: Callable,
    /// `kira_rt_cstring_new`: builds transient NUL-terminated C storage from a
    /// Kira string handle for one foreign call (null on interior NUL).
    pub(in crate::codegen) cstring_new: Callable,
    /// `kira_rt_cstring_free`: frees the storage `kira_rt_cstring_new` produced.
    pub(in crate::codegen) cstring_free: Callable,
    /// `kira_rt_ffi_call_bytes`: the shared libffi call path for native code.
    pub(in crate::codegen) ffi_call: Callable,
    /// `kira_rt_ffi_closure`: the C-callable address of one callback entry.
    pub(in crate::codegen) ffi_closure: Callable,
    /// Creates a uniquely owned NUL-terminated C block from a Kira string.
    pub(in crate::codegen) cblock_text: Callable,
    /// Creates a uniquely owned C block by copying a byte image.
    pub(in crate::codegen) cblock_bytes: Callable,
    /// Wraps a foreign pointer word in a uniquely owned C block.
    pub(in crate::codegen) cblock_alien: Callable,
    /// Resolves a C-block handle to the pointer word C reads.
    pub(in crate::codegen) cblock_word: Callable,
    /// Deep-clones one uniquely owned C-block tree.
    pub(in crate::codegen) cblock_clone: Callable,
    /// Frees one uniquely owned C-block tree.
    pub(in crate::codegen) cblock_free: Callable,
    /// Moves one child C block under a parent image at a pointer offset.
    pub(in crate::codegen) cblock_attach: Callable,
    /// Transfers a C-block tree to the retained registry.
    pub(in crate::codegen) cblock_keep: Callable,
    /// `kira_hybrid_call_runtime`: how native code reaches the VM half.
    pub(in crate::codegen) call_runtime: Callable,
    pub(in crate::codegen) native_value_int: Callable,
    pub(in crate::codegen) native_value_any: Callable,
    pub(in crate::codegen) native_value_read_any_type: Callable,
    pub(in crate::codegen) native_value_raw_ptr: Callable,
    /// A capture cell into a state node, and the box back out of one.
    pub(in crate::codegen) native_value_cell: Callable,
    pub(in crate::codegen) native_value_read_cell: Callable,
    pub(in crate::codegen) native_value_float: Callable,
    pub(in crate::codegen) native_value_bool: Callable,
    pub(in crate::codegen) native_value_string: Callable,
    /// Moves a native C-block handle into a portable state node.
    pub(in crate::codegen) native_value_cblock_from_handle: Callable,
    /// Moves a portable C-block state node into a native handle.
    pub(in crate::codegen) native_value_cblock_to_handle: Callable,
    pub(in crate::codegen) native_value_aggregate: Callable,
    pub(in crate::codegen) native_value_set_child: Callable,
    pub(in crate::codegen) native_value_read_int: Callable,
    pub(in crate::codegen) native_value_read_raw_ptr: Callable,
    pub(in crate::codegen) native_value_read_float: Callable,
    pub(in crate::codegen) native_value_read_bool: Callable,
    pub(in crate::codegen) native_value_read_string: Callable,
    pub(in crate::codegen) native_value_enum_tag: Callable,
    pub(in crate::codegen) native_value_child: Callable,
    pub(in crate::codegen) native_value_free: Callable,
    pub(in crate::codegen) native_value_array_from: Callable,
    pub(in crate::codegen) native_value_array_to: Callable,
    pub(in crate::codegen) native_state_new: Callable,
    pub(in crate::codegen) native_state_recover: Callable,
    pub(in crate::codegen) native_state_replace: Callable,
    pub(in crate::codegen) native_state_free: Callable,
    /// Allocates a box holding one state value in this backend's own layout.
    pub(in crate::codegen) native_state_box_new: Callable,
    /// The address of the value inside a box, type-checked.
    pub(in crate::codegen) native_state_box_payload: Callable,
    pub(in crate::codegen) trap_native_state: Callable,
    /// The `kira_rt_fs_*` helpers, indexed by [`FileSystemOp::as_byte`].
    ///
    /// An array rather than twelve fields, and indexed by a total function
    /// rather than searched: adding an operation adds a row to
    /// [`FileSystemOp::ALL`] and nothing here has to be remembered.
    pub(in crate::codegen) file_system: [Callable; FileSystemOp::ALL.len()],
    /// The `kira_rt_compiler_*` helpers, indexed by [`CompilerOp::as_byte`].
    ///
    /// An array for the same reason the file-system row is: the set is written
    /// down once, in [`CompilerOp::ALL`], and indexed by a total function.
    pub(in crate::codegen) compiler: [Callable; CompilerOp::ALL.len()],
    /// The `kira_rt_env_*` helpers, indexed by [`EnvOp::as_byte`].
    pub(in crate::codegen) env: [Callable; EnvOp::ALL.len()],
}
