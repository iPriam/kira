//! The `NativeBridge`: binds a compiled native library (or the current
//! process) to the VM — resolves per-function trampolines, installs the
//! runtime invoker / array allocator / closure-destroy hooks into the native
//! side, and supports live rebinding after a hot swap.
//!
//! Ported from kira-zig `packages/kira_native_bridge/src/bridge.zig`.

use std::collections::HashMap;

use kira_dynamic_ffi::DynamicLibrary;

use crate::trampoline::Trampoline;

/// Signature of the host-side runtime invoker handed to native code.
/// Zig: `RuntimeInvoker` (`fn(ctx, function_id, args, out_result)`), minus
/// the Zig error union — the port surfaces failures through the out-value.
/// TODO(port): finalize the Rust-side invoker signature with the VM port.
pub type RuntimeInvoker = unsafe extern "C" fn(
    function_id: u32,
    args: *const crate::abi::KiraBridgeValue,
    arg_count: u32,
    out_result: *mut crate::abi::KiraBridgeValue,
);

/// Binds native symbols to VM function ids and owns the installed hooks.
///
/// Zig: `NativeBridge` (allocator, optional library handle, `self_bound`,
/// trampoline table). Key behaviors that land with the port:
/// - `bind(library_path, descriptors)`: open the dylib, resolve each
///   descriptor's symbol into a trampoline, then install the runtime
///   invoker (`kira_hybrid_install_runtime_invoker`), array allocator,
///   closure-destroy thunk, and trace flag into the library.
/// - `bind_current_process(descriptors)`: same against the current image.
/// - `rebind(descriptors)`: rebuild the function-id -> trampoline table
///   against the ALREADY-LOADED library after a live hot reload — new
///   function ids, same symbols, no reopen, so sokol/graphics state and the
///   installed hooks stay valid.
#[derive(Debug, Default)]
pub struct NativeBridge {
    /// Zig: `library: ?NativeLibrary` — the bound dylib, if any.
    pub library: Option<DynamicLibrary>,
    /// Zig: `self_bound: bool` — bound against the current process image.
    pub self_bound: bool,
    /// Zig: `trampolines: AutoHashMapUnmanaged(u32, Trampoline)` — keyed by
    /// bytecode function id.
    pub trampolines: HashMap<u32, Trampoline>,
}
