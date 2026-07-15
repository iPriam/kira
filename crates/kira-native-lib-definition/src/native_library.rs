//! Native library and FFI module specifications.
//!
//! Ported from kira-zig `kira_native_lib_definition/src/native_library.zig`.

use crate::ffi_symbol::NativeSymbol;
use crate::link_extras::LinkExtras;
use crate::target_resolution::TargetSelector;

/// How a library is linked (Zig `LinkMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LinkMode {
    /// Zig `.static`.
    Static,
    /// Zig `.dynamic`.
    Dynamic,
}

/// The library's ABI (Zig `LibraryAbi`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum LibraryAbi {
    /// Zig `.c`.
    #[default]
    C,
}

/// Header exposure for binding generation (Zig `HeaderSpec`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct HeaderSpec {
    /// Zig `entrypoint: ?[]const u8`.
    pub entrypoint: Option<String>,
    /// Zig `include_dirs: []const []const u8`.
    pub include_dirs: Vec<String>,
    /// Zig `defines: []const []const u8`.
    pub defines: Vec<String>,
    /// Zig `frameworks: []const []const u8`.
    pub frameworks: Vec<String>,
    /// Zig `system_libs: []const []const u8`.
    pub system_libs: Vec<String>,
}

/// Which declarations autobinding emits (Zig `AutobindingMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AutobindingMode {
    /// Zig `.listed`.
    #[default]
    Listed,
    /// Zig `.all_public`.
    AllPublic,
}

/// Header-family profile applied by autobinding (Zig `AutobindingProfile`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AutobindingProfile {
    /// Zig `.generic`.
    #[default]
    Generic,
    /// Zig `.vulkan`.
    Vulkan,
    /// Zig `.directx12`.
    Directx12,
}

/// Binding selection for autobinding (Zig `AutobindingBindings`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AutobindingBindings {
    /// Zig `mode: AutobindingMode = .listed`.
    pub mode: AutobindingMode,
    /// Zig `profile: AutobindingProfile = .generic`.
    pub profile: AutobindingProfile,
    /// Zig `functions: []const []const u8`.
    pub functions: Vec<String>,
    /// Zig `structs: []const []const u8`.
    pub structs: Vec<String>,
    /// Zig `callbacks: []const []const u8`.
    pub callbacks: Vec<String>,
}

/// Autobinding request for a library (Zig `AutobindingSpec`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AutobindingSpec {
    /// Zig `module_name: []const u8`.
    pub module_name: String,
    /// Zig `output_path: []const u8`.
    pub output_path: String,
    /// Zig `headers: []const []const u8`.
    pub headers: Vec<String>,
    /// Zig `bindings: AutobindingBindings = .{}`.
    pub bindings: AutobindingBindings,
}

/// How the library's own sources are built (Zig `BuildRecipe`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BuildRecipe {
    /// Zig `sources: []const []const u8`.
    pub sources: Vec<String>,
    /// Zig `include_dirs: []const []const u8`.
    pub include_dirs: Vec<String>,
    /// Zig `defines: []const []const u8`.
    pub defines: Vec<String>,
}

/// Per-target artifact/link spec (Zig `TargetSpec`).
#[derive(Debug, Clone, PartialEq)]
pub struct TargetSpec {
    /// Zig `selector: TargetSelector`.
    pub selector: TargetSelector,
    /// Zig `static_lib: ?[]const u8`.
    pub static_lib: Option<String>,
    /// Zig `dynamic_lib: ?[]const u8`.
    pub dynamic_lib: Option<String>,
    /// Zig `compiler_flags` — extra flags when compiling this library's own
    /// sources for this target.
    pub compiler_flags: Vec<String>,
    /// Zig `link: LinkExtras = .{}`.
    pub link: LinkExtras,
}

/// A declared native library (Zig `NativeLibrarySpec`).
#[derive(Debug, Clone, PartialEq)]
pub struct NativeLibrarySpec {
    /// Zig `name: []const u8`.
    pub name: String,
    /// Zig `link_mode: LinkMode`.
    pub link_mode: LinkMode,
    /// Zig `abi: LibraryAbi`.
    pub abi: LibraryAbi,
    /// Zig `headers: HeaderSpec = .{}`.
    pub headers: HeaderSpec,
    /// Zig `autobinding: ?AutobindingSpec`.
    pub autobinding: Option<AutobindingSpec>,
    /// Zig `build: BuildRecipe = .{}`.
    pub build: BuildRecipe,
    /// Zig `targets: []const TargetSpec`.
    pub targets: Vec<TargetSpec>,
    /// Zig `symbols: []const NativeSymbol`.
    pub symbols: Vec<NativeSymbol>,
}

/// An FFI module: libraries plus loose symbols (Zig `FfiModuleSpec`).
#[derive(Debug, Clone, PartialEq)]
pub struct FfiModuleSpec {
    /// Zig `module_name: []const u8`.
    pub module_name: String,
    /// Zig `libraries: []const NativeLibrarySpec`.
    pub libraries: Vec<NativeLibrarySpec>,
    /// Zig `symbols: []const NativeSymbol`.
    pub symbols: Vec<NativeSymbol>,
}
