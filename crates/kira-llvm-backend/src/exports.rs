//! What a native library exports, as the backend is told it.
//!
//! # Why the symbols arrive rather than being derived
//!
//! Every name here — the marker, one per export, one per exported class — has
//! exactly one correct spelling, and `kira-main` owns it (`kira_main::abi`).
//! That crate sits at layer 5 and this one at layer 4, so the backend cannot ask
//! it; and re-deriving the same `format!` here would put a second answer in the
//! tree, which is the shape of every symbol-drift bug the marker exists to
//! prevent.
//!
//! So the caller derives each name once, from `kira-main`, and hands it down.
//! The backend's job is to emit *these* symbols, not to have an opinion about
//! what they should be called.
//!
//! # Why a class is named rather than indexed
//!
//! The consumer's export table indexes classes in first-mention order across the
//! export signatures. Recomputing that order here to turn an index back into a
//! type would be a second implementation of a rule `kira-bytecode` already
//! owns — and two implementations of an ordering is one more than can be right.
//! A class arrives by the name it was declared under, which the backend resolves
//! against the program's own struct table by a total lookup.

/// One exported function the backend emits a trampoline for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeExport {
    /// The symbol the trampoline is emitted under
    /// (`kira_lib_uifoundation_make_button`).
    pub symbol: String,
    /// Index of the exported function within `IrProgram::functions`.
    pub function: u32,
}

/// One exported class the backend synthesizes a destructor for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeClass {
    /// The class's name as the Kira author declared it (`Button`).
    pub name: String,
    /// The symbol its destructor is emitted under
    /// (`kira_lib_uifoundation_drop_button`).
    pub symbol: String,
}

/// The whole export surface of one native library.
///
/// Empty for an executable and for a hybrid half, both of which are entered
/// another way entirely.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NativeExportSurface {
    /// The per-library ABI marker symbol (`kira_lib_uifoundation_abi_1`).
    ///
    /// Emitted as an empty function the consumer's generated wrapper *calls*, so
    /// an archive built under a different export contract fails the consumer's
    /// link naming this symbol. Empty means no marker is emitted, which is what
    /// a program or a hybrid half wants.
    pub abi_marker: Option<String>,
    /// Every exported function, in declaration order.
    pub functions: Vec<NativeExport>,
    /// Every class an exported signature mentions, in the order the consumer's
    /// export table indexes them.
    pub classes: Vec<NativeClass>,
}

impl NativeExportSurface {
    /// Whether this surface asks for nothing to be emitted.
    pub fn is_empty(&self) -> bool {
        self.abi_marker.is_none() && self.functions.is_empty() && self.classes.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_program_asks_for_no_export_surface_at_all() {
        assert!(NativeExportSurface::default().is_empty());
    }

    #[test]
    fn a_marker_alone_is_still_a_surface() {
        // A library that exports nothing still defines its marker: the wrapper
        // calls it from `load()`, so a library with an empty surface and a stale
        // one must not link identically.
        let surface = NativeExportSurface {
            abi_marker: Some("kira_lib_uifoundation_abi_1".to_owned()),
            ..NativeExportSurface::default()
        };
        assert!(!surface.is_empty());
    }
}
