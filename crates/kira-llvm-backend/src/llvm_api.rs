//! Dynamically-loaded LLVM-C API surface.
//!
//! Ported from kira-zig `packages/kira_llvm_backend/src/llvm_c.zig`.
//!
//! # Design note (binding strategy — decided, lands at migration)
//!
//! NO `llvm-sys`/`inkwell` — ever. Those crates hard-require an installed
//! LLVM at build time; Kira's toolchain discovers/fetches its own LLVM-C
//! dylib at RUN time (see the `toolchain` module) and must build on machines
//! with no LLVM at all. The Rust port therefore mirrors the Zig design:
//!
//! - `libloading::Library` opens the toolchain's LLVM-C dylib
//!   (Zig: `std.DynLib` / `LoadLibraryExW` with altered search path).
//! - An `LlvmApi` struct holds one function pointer per LLVM-C entry point,
//!   each resolved by exact symbol name at open time — the Zig `Api` struct
//!   has ~130 required fields (context/module/builder lifecycle, type
//!   constructors, instruction builders, target machine + pass runner) and
//!   an OPTIONAL DWARF surface (`LLVMDIBuilder*`, nullable fields) that
//!   degrades debug-info emission with a diagnostic instead of aborting when
//!   a trimmed LLVM runtime lacks it (`hasDebugInfo`/`hasDbgDeclare`).
//! - Signatures are hand-declared `unsafe extern "C" fn` types against
//!   opaque `*mut c_void` refs (LLVM-C's `LLVMContextRef` etc. are opaque
//!   pointers), so no bindgen and no LLVM headers are needed.
//! - The full table lands behind the `llvm-dylib` cargo feature during
//!   migration; the feature gates the loader only, never a build dependency.
//! - Tests never dlopen: anything above the loader takes the table as a
//!   parameter so it can be faked.
//!
//! The struct below is a 3-field ILLUSTRATION of the shape (the real table
//! is generated field-by-field from `llvm_c.zig` at migration). It compiles
//! without LLVM installed and nothing in this crate calls `Library::new`.

use core::ffi::{c_char, c_void};

/// Opaque LLVM-C object reference (`LLVMContextRef`, `LLVMModuleRef`, ...).
pub type LlvmRef = *mut c_void;

/// Illustrative slice of the future function-pointer table. Zig: `Api`.
///
/// Symbols borrow the loaded library (`'lib`), exactly like the Zig struct
/// keeping `lib` alongside its pointers; the real table will own the
/// `libloading::Library` and self-reference via `Symbol::into_raw`.
pub struct LlvmApi<'lib> {
    /// `LLVMContextCreate: fn() -> LLVMContextRef`.
    pub context_create: libloading::Symbol<'lib, unsafe extern "C" fn() -> LlvmRef>,
    /// `LLVMContextDispose: fn(LLVMContextRef)`.
    pub context_dispose: libloading::Symbol<'lib, unsafe extern "C" fn(LlvmRef)>,
    /// `LLVMModuleCreateWithNameInContext: fn(*const c_char, LLVMContextRef) -> LLVMModuleRef`.
    pub module_create_with_name_in_context:
        libloading::Symbol<'lib, unsafe extern "C" fn(*const c_char, LlvmRef) -> LlvmRef>,
}

impl std::fmt::Debug for LlvmApi<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LlvmApi(..)")
    }
}
