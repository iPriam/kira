//! Import path resolution.
//!
//! Ported from kira-zig `kira_program_graph/src/imports.zig`. Owns
//! `resolveImportPath` (qualified module name -> candidate file paths, local
//! and package-rooted), `packageRootOwnerForImport`, `firstExistingCandidate`,
//! `resolvedCandidateNotes`, and `qualifiedNameDisplay`.

/// Outcome of resolving one import (Zig `ImportResolution`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ImportResolution {
    /// Zig `display_name: []u8` — the dotted module name for diagnostics.
    pub display_name: String,
    /// Zig `candidates: [][]u8` — candidate file paths, in probe order.
    pub candidates: Vec<String>,
    /// Zig `exists: bool` — whether any candidate exists on disk.
    pub exists: bool,
}

// TODO(port): `resolve_import_path(source_path, module_name, module_map)`,
// `package_root_owner_for_import`, `first_existing_candidate`,
// `resolved_candidate_notes`, `qualified_name_display` — blocked on the
// `kira-syntax-model` QualifiedName and `kira-package-manager` ModuleMap
// types (sibling skeletons at scaffold time).
