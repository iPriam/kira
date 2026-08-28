//! LLVM/native backend: compiles Kira IR to machine code via LLVM.
//!
//! Layer 4 of the Kira package graph.
//!
//! # Shape
//!
//! The backend consumes the same verified [`IrProgram`] the VM's bytecode
//! compiler does and lowers it *in process* through the LLVM C API
//! (`llvm-sys`), emitting a real object file with LLVM's own target machine —
//! no textual-IR round trip and no `clang -x ir` subprocess in the codegen
//! path. `clang` from the managed toolchain is used only as the linker driver.
//!
//! # Parity
//!
//! Native code must behave exactly as the VM does on the same program, so the
//! lowering mirrors the interpreter's semantics rather than taking the
//! shortcuts a C compiler would:
//!
//! - integer arithmetic wraps (no `nsw`/`nuw`), matching the VM's `wrapping_*`,
//! - `/` and `%` by zero call the runtime's trap helper, and `MIN / -1` is
//!   special-cased to the wrapping result instead of LLVM's poison,
//! - `print` and all string work go through the stable `kira_rt_*` helpers in
//!   `kira-native-bridge`, which format with the same standard library the VM
//!   uses — so output is identical byte-for-byte.
//!
//! # Standing decisions
//!
//! - LLVM is reached through `llvm-sys`, never `inkwell`.
//! - LLVM is a hard dependency: every build of this crate carries the backend,
//!   and a machine without a managed LLVM does not build the workspace. The
//!   `verifying-work` provisioning notes name the fix.
//! - `unsafe` is fenced to this crate's binding layer, with a `// SAFETY:`
//!   comment on every block.

use kira_backend_api::NativeTarget;
use kira_runtime_abi::ForeignSignature;

// Re-exported because it is the type of a public option field: a caller
// building a `NativeBuildOptions` must be able to name it without taking a
// dependency of its own on the model crate.
pub use kira_native_lib_definition::NativeLinkInputs;

mod build;
mod codegen;
mod error;
mod exports;
#[cfg(test)]
mod foreign_integration_tests;
mod hybrid;
mod link;
// Not gated: the platform link list is data about a host rather than something
// LLVM answers, and a consumer's build script reads it on a machine with none.
mod options;
mod platform;
mod reachability;
pub mod shim;
// Public because a shim object is an input to a link line rather than an
// internal step: whoever assembles that line — this crate for a native program,
// the CLI for the emscripten link — needs to name the object it produced. It
// was neither declared nor compiled until the target-aware link went in, which
// is also why its own test had never run.
pub mod shim_build;
#[cfg(test)]
mod shim_tests;

pub use build::{
    build_native, build_native_debug, build_native_library, build_native_live, build_wasm_library,
    build_wasm_object,
};
pub use error::LlvmError;
pub use exports::{NativeClass, NativeExport, NativeExportSurface};
pub use hybrid::{
    HybridArtifacts, build_hybrid_library, build_hybrid_library_debug, build_hybrid_object,
    has_reachable_hybrid_native_functions, hybrid_uses_compiler_runtime,
};
pub use link::{LinkError, NativeBuildTarget, SYSROOT_VARIABLE, link_ffi_carrier};
pub use options::{NativeArtifacts, NativeBuildOptions};
pub use platform::{PLATFORM_LINK_LISTS, PlatformLinkList, host_link_list, link_list_for};

/// The infix marking a partially written object: `<object>.pending-<pid>`.
///
/// A build emits each object under this name and renames it onto the final path,
/// so the final path only ever holds a finished object. A build killed mid-emit
/// never runs that rename, leaving the partial behind. Exposed so the builder
/// that next holds the package's build lock can recognise and sweep the partials
/// an interrupted build abandoned, rather than a person reaching for `rm`.
pub const PENDING_INFIX: &str = ".pending-";

/// Reports whether this compiler can emit machine code for `target`.
///
/// What a compiler can emit for is fixed when it is linked: the managed LLVM
/// bundle carries a set of code generators, and one it was not built with is a
/// set of symbols that are simply not in the binary. That is knowable before a
/// program is read, which is why this exists separately from the build — a
/// caller asks it first, and a machine that cannot serve the request says so
/// before the user goes and arranges a sysroot and a runtime archive for a build
/// that could never have finished.
///
/// [`NativeTarget::Host`] is always supported: a bundle without this host's own
/// code generator is refused by the backend's build script, since it could emit
/// for nothing at all.
pub fn supports_target(target: &NativeTarget) -> Result<(), LlvmError> {
    match target.cross() {
        None => Ok(()),
        Some(cross) => codegen::check_supported(cross),
    }
}

/// The symbol a foreign import's adapter is bound under.
///
/// The spelling lives in the runtime ABI crate and is what a hybrid manifest
/// records per import. The LLVM backend does **not** define this symbol: both
/// live hosts bind imports through libffi closures generated at load time,
/// which carry these names. An emitter that wants to define adapters natively
/// again owns this contract afresh.
pub fn adapter_name(index: usize) -> String {
    kira_runtime_abi::foreign_adapter_name(index)
}

/// The exported symbol C holds for callback `index`.
///
/// The other half of the same wire contract [`adapter_name`] carries: the
/// backend defines this symbol, and the VM's host resolves it by name to get the
/// address a `@FFI.Callback` value holds.
///
/// *Which* half of the build defines it depends on the signature. A scalar-only
/// callback is entered directly, so LLVM emits this symbol itself. One whose
/// signature takes a struct by value cannot be: only a C compiler knows how that
/// struct arrives, so the generated shim defines this symbol with the true C
/// prototype and forwards to [`callback_body_name`]. Either way the address C
/// holds is this name, which is why no host has to know the difference.
pub fn callback_name(index: usize) -> String {
    kira_runtime_abi::foreign_callback_name(index)
}

/// The symbol LLVM's entry thunk for callback `index` is defined under when the
/// generated shim owns [`callback_name`].
///
/// Never the address C holds: the shim's entry is, and this is what it calls
/// with each by-value struct replaced by its address.
pub fn callback_body_name(index: usize) -> String {
    format!("kira_ffi_callback_body_{index}")
}

/// The symbol LLVM defines the entry thunk for callback `index` under.
///
/// [`callback_name`] for a signature LLVM can present to C on its own, and
/// [`callback_body_name`] for one whose by-value struct the shim classifies —
/// the same split [`crate::shim::callback_needs_entry`], so the name emitted
/// here and the entry the shim generates always agree.
pub fn callback_thunk_symbol(index: usize, signature: &ForeignSignature) -> String {
    if shim::callback_needs_entry(signature) {
        callback_body_name(index)
    } else {
        callback_name(index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The adapter symbol is a wire contract shared with the VM host and the
    /// hybrid manifest, so it is pinned rather than only round-tripped.
    #[test]
    fn adapter_symbols_are_pinned_per_import_index() {
        assert_eq!(adapter_name(0), "kira_foreign_adapter_0");
        assert_eq!(adapter_name(7), "kira_foreign_adapter_7");
    }

    #[test]
    fn callback_body_symbols_are_reserved_only_for_aggregate_entries() {
        let scalar = ForeignSignature::scalars(
            [kira_runtime_abi::ForeignType::I32],
            kira_runtime_abi::ForeignType::Void,
        );
        assert_eq!(callback_thunk_symbol(2, &scalar), "kira_ffi_callback_2");

        let aggregate = ForeignSignature::new(
            [kira_runtime_abi::ForeignTypeSpec::Aggregate(
                kira_runtime_abi::ForeignAggregateId(0),
            )],
            kira_runtime_abi::ForeignType::Void,
        );
        assert_eq!(
            callback_thunk_symbol(2, &aggregate),
            "kira_ffi_callback_body_2"
        );
    }
}
