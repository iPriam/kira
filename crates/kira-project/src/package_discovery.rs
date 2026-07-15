//! Manifest discovery: file names and target resolution from paths.
//!
//! Ported from kira-zig `kira_project/src/package_discovery.zig`. The
//! load/resolve functions land with the port; the manifest naming constants
//! are the stable surface.

/// The declaration manifest. It takes precedence over `kira.toml` when both
/// are present in a package directory (it is first in
/// [`MANIFEST_FILE_NAMES`]).
pub const DECLARATION_MANIFEST_FILE_NAME: &str = "package.kira";
pub const PREFERRED_MANIFEST_FILE_NAME: &str = "kira.toml";
pub const LEGACY_MANIFEST_FILE_NAME: &str = "project.toml";
pub const REPO_MANIFEST_FILE_NAME: &str = "Kira.toml";
pub const MANIFEST_FILE_NAME: &str = PREFERRED_MANIFEST_FILE_NAME;
pub const ENTRYPOINT_REL_PATH: &str = "app/main.kira";

/// All accepted manifest file names, in precedence order.
pub const MANIFEST_FILE_NAMES: [&str; 4] = [
    DECLARATION_MANIFEST_FILE_NAME,
    PREFERRED_MANIFEST_FILE_NAME,
    LEGACY_MANIFEST_FILE_NAME,
    REPO_MANIFEST_FILE_NAME,
];

/// True when the manifest at `path` is a `package.kira` declaration manifest.
pub fn is_declaration_manifest(path: &str) -> bool {
    std::path::Path::new(path)
        .file_name()
        .is_some_and(|name| name == DECLARATION_MANIFEST_FILE_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declaration_manifest_wins_precedence() {
        assert_eq!(DECLARATION_MANIFEST_FILE_NAME, MANIFEST_FILE_NAMES[0]);
        assert!(is_declaration_manifest("some/dir/package.kira"));
        assert!(!is_declaration_manifest("some/dir/kira.toml"));
    }
}
