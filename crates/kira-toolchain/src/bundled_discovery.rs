//! Finding the packages that ship *with* the toolchain rather than with a
//! project.
//!
//! Foundation is the standard library: `import Foundation` names it from any
//! project, with no path and no dependency entry, because a copy of it is
//! installed beside the compiler and versioned with it. This module answers the
//! one question that needs a disk — *where is that copy* — and nothing else.
//! What is inside it (its manifest, its module root, its files) is read by
//! `kira-program-graph`, which is high enough to depend on the manifest reader.
//!
//! # The shipped layout is the binary's neighbour
//!
//! An installed toolchain is `<root>/bin/kirac` and `<root>/foundation/`, so
//! the primary rule resolves the bundle **relative to the running executable**
//! and never consults `$HOME`, `current.toml`, or the working directory. That
//! is what makes a toolchain relocatable: move the whole directory and the
//! stdlib moves with it, still matching the compiler that was installed with
//! it. A version-skewed pairing of one toolchain's `kirac` with another's
//! Foundation cannot be reached by this rule at all.
//!
//! # Discovery is explicit and ordered
//!
//! 1. `KIRA_FOUNDATION_HOME` — an explicit override always wins and never
//!    falls through, exactly as `KIRA_LLVM_HOME` does in
//!    [`crate::llvm_discovery`].
//! 2. `<exe-dir>/../foundation` — the shipped layout above.
//! 3. The active managed toolchain named by `~/.kira/toolchains/current.toml`.
//!    This is the route for a *consumer* that is not `kirac` itself — a
//!    `build.rs` compiling a Kira library through `kira-build`, whose
//!    executable is a Cargo build script sitting nowhere near a toolchain.
//! 4. A source checkout: walking up from the executable for a directory that
//!    holds both a workspace `Cargo.toml` and `foundation/package.kira`.
//!
//! Rule 4 is the developer's rule and only the developer's: it is reached only
//! after rules 2 and 3 both failed, so a shipped toolchain never depends on a
//! checkout existing, and a `kirac` built into `target/debug/` still finds the
//! `foundation/` that is committed in the repo it was built from.

use std::path::{Path, PathBuf};

use crate::{Channel, CurrentToolchain, current_toolchain_path, managed_toolchain_root};

/// The directory name a bundled Foundation carries under a toolchain root.
const FOUNDATION_DIR_NAME: &str = "foundation";

/// The manifest file that marks a directory as a Kira package.
const PACKAGE_MANIFEST_FILE_NAME: &str = "package.kira";

/// The subdirectory of a package holding its Kira sources.
const PACKAGE_SOURCE_DIR_NAME: &str = "app";

/// A bundled package found on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundledPackage {
    /// The package root — the directory holding `package.kira`.
    pub root: PathBuf,
    /// Which rule in the discovery order selected this tree.
    pub source: BundledSource,
}

impl BundledPackage {
    /// The package's `package.kira`.
    #[must_use]
    pub fn manifest_path(&self) -> PathBuf {
        self.root.join(PACKAGE_MANIFEST_FILE_NAME)
    }

    /// The directory the package's modules are resolved under.
    #[must_use]
    pub fn source_dir(&self) -> PathBuf {
        self.root.join(PACKAGE_SOURCE_DIR_NAME)
    }
}

/// Which discovery rule produced a [`BundledPackage`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BundledSource {
    /// The `KIRA_FOUNDATION_HOME` override.
    EnvironmentOverride,
    /// The shipped layout: a sibling of the directory holding the executable.
    ShippedBesideBinary,
    /// The toolchain named by `~/.kira/toolchains/current.toml`.
    ManagedToolchain,
    /// A source checkout above the running executable.
    RepoCheckout,
}

/// Why a bundled package could not be found.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BundledDiscoveryError {
    /// `KIRA_FOUNDATION_HOME` was set but does not name a Kira package.
    #[error(
        "KIRA_FOUNDATION_HOME is set to `{path}` but that is not a Kira package \
         (no `package.kira` with an `app` directory beside it); point it at a \
         Foundation package root or unset it to use the bundled one"
    )]
    OverrideUnusable {
        /// The path the override named.
        path: PathBuf,
    },
    /// Nothing was found anywhere in the discovery order.
    #[error(
        "no bundled `{name}` package found; checked:\n{}\n\
         set KIRA_FOUNDATION_HOME to a Foundation package root, or reinstall the \
         toolchain so that `{name}/` sits beside its `bin/`",
        .checked.iter().map(|path| format!("  {}", path.display())).collect::<Vec<_>>().join("\n")
    )]
    NotFound {
        /// The package directory name that was looked for.
        name: String,
        /// Every path that was checked, in discovery order.
        checked: Vec<PathBuf>,
    },
}

/// Resolves the bundled Foundation package.
///
/// Uses the running executable's own path as the anchor for the shipped and
/// checkout rules. An executable path that cannot be determined skips those two
/// rules rather than failing: the managed-toolchain rule may still answer.
pub fn discover_foundation() -> Result<BundledPackage, BundledDiscoveryError> {
    discover_foundation_from(std::env::current_exe().ok().as_deref())
}

/// Resolves the bundled Foundation package, anchoring the executable-relative
/// rules at `executable` instead of at this process's own path.
///
/// Split out from [`discover_foundation`] so the shipped layout is testable:
/// the rule that matters most is the one no test could exercise if the anchor
/// were always the test harness's own binary.
pub fn discover_foundation_from(
    executable: Option<&Path>,
) -> Result<BundledPackage, BundledDiscoveryError> {
    let mut checked = Vec::new();

    // 1. An explicit override always wins, and never falls through: if the user
    //    named a tree and it is wrong, say so rather than silently using another.
    if let Some(root) = env_override() {
        return if is_package_root(&root) {
            Ok(BundledPackage {
                root,
                source: BundledSource::EnvironmentOverride,
            })
        } else {
            Err(BundledDiscoveryError::OverrideUnusable { path: root })
        };
    }

    let exe_dir = executable.and_then(Path::parent);

    // 2. The shipped layout: `<root>/bin/kirac` next to `<root>/foundation`.
    if let Some(root) = exe_dir.and_then(Path::parent) {
        let candidate = root.join(FOUNDATION_DIR_NAME);
        if is_package_root(&candidate) {
            return Ok(BundledPackage {
                root: candidate,
                source: BundledSource::ShippedBesideBinary,
            });
        }
        checked.push(candidate);
    }

    // 3. The toolchain `current.toml` points at, for a consumer that is not the
    //    compiler binary.
    if let Some(candidate) = managed_foundation_root() {
        if is_package_root(&candidate) {
            return Ok(BundledPackage {
                root: candidate,
                source: BundledSource::ManagedToolchain,
            });
        }
        checked.push(candidate);
    }

    // 4. A source checkout, last and only for a developer build.
    if let Some(dir) = exe_dir
        && let Some(candidate) = checkout_foundation_root(dir)
    {
        return Ok(BundledPackage {
            root: candidate,
            source: BundledSource::RepoCheckout,
        });
    }

    Err(BundledDiscoveryError::NotFound {
        name: FOUNDATION_DIR_NAME.to_owned(),
        checked,
    })
}

/// The `KIRA_FOUNDATION_HOME` override, when set to a non-empty value.
fn env_override() -> Option<PathBuf> {
    let value = std::env::var_os("KIRA_FOUNDATION_HOME")?;
    (!value.is_empty()).then(|| PathBuf::from(value))
}

/// Where the toolchain named by `current.toml` keeps its Foundation.
///
/// `None` when there is no home directory, no `current.toml`, or one that
/// cannot be parsed — all of which mean "no managed toolchain is active", which
/// is a fall-through rather than an error.
fn managed_foundation_root() -> Option<PathBuf> {
    let state = current_toolchain_path().ok()?;
    let text = std::fs::read_to_string(state).ok()?;
    let current = CurrentToolchain::parse_toml(&text).ok()?;
    managed_foundation_root_for(current.channel, &current.version)
}

/// Where a named managed toolchain keeps its Foundation.
fn managed_foundation_root_for(channel: Channel, version: &str) -> Option<PathBuf> {
    Some(
        managed_toolchain_root(channel, version)
            .ok()?
            .join(FOUNDATION_DIR_NAME),
    )
}

/// Walks up from `start` for a source checkout carrying a committed Foundation.
///
/// Both markers are required. A workspace `Cargo.toml` alone would match any
/// Rust project the compiler happened to be built inside, and a bare
/// `foundation/` alone would match a user directory that happens to be called
/// that — neither is a Kira checkout, and binding to one silently would be
/// worse than not finding a Foundation at all.
fn checkout_foundation_root(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(current) = dir {
        let candidate = current.join(FOUNDATION_DIR_NAME);
        if current.join("Cargo.toml").is_file() && is_package_root(&candidate) {
            return Some(candidate);
        }
        dir = current.parent();
    }
    None
}

/// Whether `root` is the root of a Kira package with sources in it.
///
/// The manifest is what makes a directory a package; the source directory is
/// what makes it one worth resolving imports against. A `foundation/` holding
/// only a manifest resolves no module, so treating it as found would turn a
/// broken install into a pile of unresolved-import diagnostics instead of one
/// message naming the install.
fn is_package_root(root: &Path) -> bool {
    root.join(PACKAGE_MANIFEST_FILE_NAME).is_file() && root.join(PACKAGE_SOURCE_DIR_NAME).is_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory that removes itself.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> TempDir {
            let base = std::env::temp_dir().join(format!(
                "kira-bundled-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id(),
            ));
            let _ = std::fs::remove_dir_all(&base);
            std::fs::create_dir_all(&base).expect("a scratch directory");
            TempDir(base)
        }

        /// Writes a minimal Foundation package at `<base>/<relative>`.
        fn write_package(&self, relative: &str) -> PathBuf {
            let root = self.0.join(relative);
            std::fs::create_dir_all(root.join(PACKAGE_SOURCE_DIR_NAME)).expect("package dirs");
            std::fs::write(
                root.join(PACKAGE_MANIFEST_FILE_NAME),
                "Package Foundation {\n    let moduleRoot = \"Foundation\"\n}\n",
            )
            .expect("write manifest");
            root
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The rule that matters on a shipped toolchain: `bin/kirac` finds the
    /// `foundation/` that is its own directory's sibling, with no home
    /// directory, no `current.toml`, and no checkout in play.
    #[test]
    fn the_shipped_layout_resolves_beside_the_binary() {
        let dir = TempDir::new("shipped");
        let expected = dir.write_package("toolchain/foundation");
        let exe = dir.0.join("toolchain/bin/kirac");
        std::fs::create_dir_all(exe.parent().expect("bin dir")).expect("bin dir");

        let found = discover_foundation_from(Some(&exe)).expect("the shipped Foundation");
        assert_eq!(found.root, expected);
        assert_eq!(found.source, BundledSource::ShippedBesideBinary);
        assert_eq!(found.manifest_path(), expected.join("package.kira"));
        assert_eq!(found.source_dir(), expected.join("app"));
    }

    /// A directory named `foundation` with no manifest is not a package, so the
    /// shipped rule declines it rather than resolving imports against nothing.
    #[test]
    fn a_directory_without_a_manifest_is_not_a_package() {
        let dir = TempDir::new("bare");
        std::fs::create_dir_all(dir.0.join("toolchain/foundation/app")).expect("dirs");
        assert!(!is_package_root(&dir.0.join("toolchain/foundation")));
    }

    /// A package with a manifest but no `app/` is equally not resolvable.
    #[test]
    fn a_package_without_sources_is_not_a_package_root() {
        let dir = TempDir::new("nosrc");
        let root = dir.0.join("toolchain/foundation");
        std::fs::create_dir_all(&root).expect("dirs");
        std::fs::write(root.join("package.kira"), "Package Foundation {}\n").expect("manifest");
        assert!(!is_package_root(&root));
    }

    /// The developer's rule: a `kirac` built into `target/debug/` walks up to
    /// the checkout that holds both a workspace `Cargo.toml` and `foundation/`.
    #[test]
    fn a_source_checkout_resolves_by_walking_up_from_the_binary() {
        let dir = TempDir::new("checkout");
        let expected = dir.write_package("repo/foundation");
        std::fs::write(dir.0.join("repo/Cargo.toml"), "[workspace]\n").expect("workspace manifest");
        let exe_dir = dir.0.join("repo/target/debug");
        std::fs::create_dir_all(&exe_dir).expect("target dir");

        let found = checkout_foundation_root(&exe_dir).expect("the checkout Foundation");
        assert_eq!(found, expected);
    }

    /// Both markers are required: a `foundation/` with no workspace manifest
    /// above it is somebody else's directory.
    #[test]
    fn a_foundation_without_a_workspace_manifest_is_not_a_checkout() {
        let dir = TempDir::new("nomarker");
        dir.write_package("repo/foundation");
        let exe_dir = dir.0.join("repo/target/debug");
        std::fs::create_dir_all(&exe_dir).expect("target dir");
        assert_eq!(checkout_foundation_root(&exe_dir), None);
    }

    /// The managed route builds the path `current.toml` describes.
    #[test]
    fn the_managed_route_is_the_current_toolchain_root() {
        let Some(root) = managed_foundation_root_for(Channel::Dev, "1.7.3") else {
            return; // no home directory in this environment
        };
        let expected: PathBuf = [".kira", "toolchains", "dev", "1.7.3", "foundation"]
            .iter()
            .collect();
        assert!(root.ends_with(&expected), "{}", root.display());
    }

    /// Nothing anywhere reports every path it looked at, so a user can see
    /// where the install was expected to be.
    #[test]
    fn nothing_found_names_the_paths_it_checked() {
        let dir = TempDir::new("missing");
        let exe = dir.0.join("empty/bin/kirac");
        std::fs::create_dir_all(exe.parent().expect("bin dir")).expect("bin dir");
        // Only meaningful when this machine has no managed toolchain to fall
        // back to; where it has one, discovery legitimately succeeds.
        if let Err(error) = discover_foundation_from(Some(&exe)) {
            let BundledDiscoveryError::NotFound { checked, .. } = error else {
                panic!("expected a not-found error");
            };
            assert!(
                checked.contains(&dir.0.join("empty/foundation")),
                "{checked:?}"
            );
        }
    }
}
