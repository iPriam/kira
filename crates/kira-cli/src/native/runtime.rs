//! Locating the Kira runtime archive a native build links against.
//!
//! One question with two very different answers. For this machine it is a file
//! cargo has already written beside the compiler, and the only difficulty is
//! which of the two archives a program needs and which of cargo's two layouts
//! it landed in. For another machine there is no such file unless somebody made
//! one, so the whole of this half is about looking in the places it could be and
//! saying exactly what to run when it is in none of them.

use std::path::{Path, PathBuf};

use kira_backend_api::{CrossTarget, NativeTarget};
use kira_ir::IrProgram;

use super::NativeError;

/// Locates the native runtime archive `program` needs.
///
/// Two archives, and the program picks. The base one carries the runtime every
/// native program needs; `libkira_compiler_bridge.a` carries that *and* the
/// check-only frontend, because native code has no host to ask for a compiler
/// and can only reach one that was linked in. Linking the larger one always
/// would put a compiler inside every program Kira ever produces, and linking
/// both is not possible — two Rust static libraries in one link line duplicate
/// the standard library — so the answer is whichever one this program needs.
///
/// Cargo writes a workspace member's staticlib beside the executable, while a
/// package-only build may leave a hashed copy under `target/<profile>/deps/`.
/// Accept both layouts so `cargo build -p kira-cli` and a workspace build have
/// the same runtime behavior.
pub fn runtime_archive(program: &IrProgram, target: &NativeTarget) -> Result<PathBuf, NativeError> {
    runtime_archive_for(program.uses_compiler(), target)
}

/// Locates the runtime archive needed by an application hybrid half.
///
/// Only compiler expressions in reachable native bodies require the compiler
/// bridge; runtime-owned compiler calls stay in the VM half. A hybrid half is
/// loaded by the interpreter in this process, so it is always this machine's.
pub fn hybrid_runtime_archive(program: &IrProgram) -> Result<PathBuf, NativeError> {
    runtime_archive_for(
        kira_llvm_backend::hybrid_uses_compiler_runtime(program),
        &NativeTarget::Host,
    )
}

fn runtime_archive_for(uses_compiler: bool, target: &NativeTarget) -> Result<PathBuf, NativeError> {
    let executable =
        std::env::current_exe().map_err(|source| NativeError::RuntimeArchive { source })?;
    let directory = executable
        .parent()
        .ok_or_else(|| NativeError::RuntimeArchive {
            source: std::io::Error::other("this executable has no parent directory"),
        })?;
    let Some(cross) = target.cross() else {
        let name = archive_file_name(uses_compiler);
        return Ok(find_runtime_archive(directory, name).unwrap_or_else(|| directory.join(name)));
    };
    cross_runtime_archive(directory, uses_compiler, cross)
}

/// Locates the runtime archive built for a machine that is not this one.
///
/// # Why this cannot fall back to the host's copy
///
/// `libkira_native_bridge.a` is a Rust `staticlib`: it carries the Rust standard
/// library, and that is machine code for whichever target cargo built it for.
/// The host's copy is x86-64 Windows objects in an aarch64 Linux link, which the
/// linker rejects — and would be a program that could not possibly run even if
/// it did not. So there is no default and no guess: either an archive built for
/// this target is found, or the build says so and says what to run.
///
/// Three places are looked in, in the order a machine is likely to have one:
///
/// 1. `KIRA_NATIVE_BRIDGE_<TARGET>`, an outright answer for a bridge that lives
///    somewhere this knows nothing about — a container image, a package
///    manager's tree, a prebuilt drop.
/// 2. `<bin>/<rust-triple>/`, a sidecar directory beside the compiler. This is
///    where an installed toolchain would carry per-target archives, next to the
///    host ones it already ships.
/// 3. `<bin>/../<rust-triple>/<profile>/`, which is where `cargo build --target`
///    writes when the compiler being run is the one in this workspace's
///    `target/<profile>/`. That is the layout a contributor building Kira from
///    source already has, so the common case needs no configuration at all.
fn cross_runtime_archive(
    directory: &Path,
    uses_compiler: bool,
    cross: &CrossTarget,
) -> Result<PathBuf, NativeError> {
    let rust_target = cross.normalized_triple();
    let crate_name = archive_crate_name(uses_compiler);
    let variable = cross_archive_variable(&rust_target);
    if let Some(named) = std::env::var_os(&variable) {
        let path = PathBuf::from(named);
        if path.is_file() {
            return Ok(path);
        }
        return Err(NativeError::CrossRuntimeArchive(Box::new(
            MissingCrossRuntimeArchive {
                target: cross.triple().to_string(),
                archive: cross_archive_file_name(crate_name, cross),
                crate_name,
                rust_target,
                searched: vec![path],
                variable,
            },
        )));
    }

    let name = cross_archive_file_name(crate_name, cross);
    let mut searched = vec![directory.join(&rust_target).join(&name)];
    // `<bin>/..` is `target/` for a compiler built in this workspace, which is
    // exactly where cargo puts a `--target` build's own profile directory.
    if let Some(parent) = directory.parent()
        && let Some(profile) = directory.file_name()
    {
        searched.push(parent.join(&rust_target).join(profile).join(&name));
    }
    if let Some(found) = searched.iter().find(|path| path.is_file()) {
        return Ok(found.clone());
    }
    Err(NativeError::CrossRuntimeArchive(Box::new(
        MissingCrossRuntimeArchive {
            target: cross.triple().to_string(),
            archive: name,
            crate_name,
            rust_target,
            searched,
            variable,
        },
    )))
}

/// The environment variable that names the runtime archive for one target.
///
/// Per target rather than one variable for all of them: a machine that
/// cross-compiles to two targets has two archives, and a single setting would
/// make the second build link the first target's.
fn cross_archive_variable(rust_target: &str) -> String {
    format!(
        "KIRA_NATIVE_BRIDGE_{}",
        rust_target.replace('-', "_").to_uppercase()
    )
}

/// The archive's file name as cargo writes it for a cross target.
///
/// The spelling is the *target toolchain's*, not this machine's: MSVC writes
/// `<name>.lib` and everything else — including Windows' own GNU toolchain —
/// writes `lib<name>.a`. Reading this off the host is how the search looks for
/// `kira_native_bridge.lib` under an `aarch64-unknown-linux-gnu` directory
/// holding a perfectly good `libkira_native_bridge.a`.
fn cross_archive_file_name(crate_name: &str, cross: &CrossTarget) -> String {
    if cross.triple().os() == "windows" && cross.triple().abi() == "msvc" {
        return format!("{crate_name}.lib");
    }
    format!("lib{crate_name}.a")
}

/// Which workspace member produces the archive a program needs.
fn archive_crate_name(uses_compiler: bool) -> &'static str {
    if uses_compiler {
        "kira_compiler_bridge"
    } else {
        "kira_native_bridge"
    }
}

/// Finds an un-hashed profile artifact first, then the newest hashed staticlib
/// Cargo placed in `deps/` when the runtime was built as a dependency.
fn find_runtime_archive(directory: &Path, name: &str) -> Option<PathBuf> {
    let direct = directory.join(name);
    if direct.is_file() {
        return Some(direct);
    }

    let expected = Path::new(name);
    let stem = expected.file_stem()?.to_str()?;
    let extension = expected.extension()?.to_str()?;
    let prefix = format!("{stem}-");
    let dependencies = directory.join("deps");
    let mut candidates: Vec<PathBuf> = std::fs::read_dir(dependencies)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path.extension().and_then(|value| value.to_str()) == Some(extension)
                && path
                    .file_stem()
                    .and_then(|value| value.to_str())
                    .is_some_and(|value| value.starts_with(&prefix))
        })
        .collect();
    candidates.sort_by_key(|path| {
        std::fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .ok()
    });
    candidates.pop()
}

/// Which archive file a program needs, by name.
///
/// Split from the path so a test can assert the choice without a built `kira`
/// beside it.
///
/// The spelling is the host toolchain's, because the file being named is one
/// cargo just wrote for this host: MSVC writes `<name>.lib` and everything else
/// writes `lib<name>.a`. Naming it the Unix way on Windows looks for a file
/// cargo never produced, which is the "native runtime archive is missing" error
/// with nothing missing.
fn archive_file_name(uses_compiler: bool) -> &'static str {
    let crate_name = archive_crate_name(uses_compiler);
    match (crate_name, cfg!(target_env = "msvc")) {
        ("kira_compiler_bridge", true) => "kira_compiler_bridge.lib",
        ("kira_compiler_bridge", false) => "libkira_compiler_bridge.a",
        (_, true) => "kira_native_bridge.lib",
        (_, false) => "libkira_native_bridge.a",
    }
}

/// Everything a cross build needs said when it cannot find its runtime archive.
///
/// A struct rather than a variant's fields because it is what
/// [`NativeError::CrossRuntimeArchive`] boxes; the message is here, where the
/// data it renders is.
#[derive(Debug, thiserror::Error)]
#[error(
    "no Kira runtime archive built for `{target}` was found\n\
     note: a cross build links the runtime for the machine it emits for, and \
     `{archive}` carries the Rust standard library as that machine's code\n\
     note: build it with `cargo build -p {package} --target {rust_target}`, \
     then put it at `{first_searched}` or name it in `{variable}`\n\
     note: looked in {searched}",
    package = crate_name.replace('_', "-"),
    first_searched = searched.first().map_or_else(String::new, |path| path.display().to_string()),
    searched = searched
        .iter()
        .map(|path| format!("`{}`", path.display()))
        .collect::<Vec<_>>()
        .join(", "),
)]
pub struct MissingCrossRuntimeArchive {
    /// The target the archive was needed for, in Kira's spelling.
    pub target: String,
    /// The archive's file name on that target.
    pub archive: String,
    /// The workspace member that produces it, as a crate name.
    pub crate_name: &'static str,
    /// The Rust target triple it must be built for.
    pub rust_target: String,
    /// Every path that was looked in, first choice first.
    pub searched: Vec<PathBuf>,
    /// The environment variable that names it outright.
    pub variable: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use kira_backend_api::RelocationModel;
    use kira_native_lib_definition::TargetTriple;

    fn aarch64() -> CrossTarget {
        CrossTarget::new(
            TargetTriple::parse("aarch64-linux-gnu").expect("a valid triple"),
            RelocationModel::Pic,
        )
    }

    /// A program that never checks a package links the small archive; one that
    /// does links the archive that carries a compiler. Both, never neither and
    /// never both — two Rust static libraries in one link line do not link.
    #[test]
    fn the_archive_a_program_links_follows_from_whether_it_checks_packages() {
        // Asserted against the host's own spelling: the name has to be the one
        // cargo wrote next to this binary, so a test that pinned the Unix name
        // everywhere would pass on the platform where the name is wrong.
        if cfg!(target_env = "msvc") {
            assert_eq!(archive_file_name(false), "kira_native_bridge.lib");
            assert_eq!(archive_file_name(true), "kira_compiler_bridge.lib");
        } else {
            assert_eq!(archive_file_name(false), "libkira_native_bridge.a");
            assert_eq!(archive_file_name(true), "libkira_compiler_bridge.a");
        }
    }

    /// The whole point of the cross variant: the message names the command that
    /// produces what is missing, with the target already filled in. A reader who
    /// has to work out the crate name and the toolchain triple from a missing
    /// file is doing the compiler's job for it.
    #[test]
    fn a_missing_cross_runtime_archive_names_the_exact_command_to_run() {
        let error = cross_runtime_archive(Path::new("/nowhere/target/debug"), false, &aarch64())
            .expect_err("no aarch64 runtime archive is installed there");
        let text = error.to_string();
        assert!(text.contains("aarch64-linux-gnu"), "{text}");
        assert!(
            text.contains("cargo build -p kira-native-bridge --target aarch64-unknown-linux-gnu"),
            "{text}",
        );
        assert!(text.contains("libkira_native_bridge.a"), "{text}");
        assert!(
            text.contains("KIRA_NATIVE_BRIDGE_AARCH64_UNKNOWN_LINUX_GNU"),
            "{text}",
        );
    }

    /// A cross build looks where a contributor's `cargo build --target` already
    /// wrote, and beside the compiler where an installed toolchain would carry
    /// per-target archives. Both, so neither arrangement needs configuring.
    #[test]
    fn a_cross_archive_is_looked_for_beside_the_compiler_and_in_cargos_target_directory() {
        let error = cross_runtime_archive(Path::new("/nowhere/target/debug"), false, &aarch64())
            .expect_err("no aarch64 runtime archive is installed there");
        let NativeError::CrossRuntimeArchive(missing) = error else {
            panic!("a missing cross archive is its own failure");
        };
        let rendered: Vec<String> = missing
            .searched
            .iter()
            .map(|path| path.display().to_string().replace('\\', "/"))
            .collect();
        assert!(
            rendered.contains(
                &"/nowhere/target/debug/aarch64-unknown-linux-gnu/libkira_native_bridge.a"
                    .to_owned()
            ),
            "{rendered:?}",
        );
        assert!(
            rendered.contains(
                &"/nowhere/target/aarch64-unknown-linux-gnu/debug/libkira_native_bridge.a"
                    .to_owned()
            ),
            "{rendered:?}",
        );
    }

    #[test]
    fn a_dependency_build_can_fall_back_to_a_hashed_profile_archive() {
        let directory = std::env::temp_dir().join(format!(
            "kira-runtime-archive-fallback-{}",
            std::process::id()
        ));
        let dependencies = directory.join("deps");
        std::fs::create_dir_all(&dependencies).expect("runtime archive test directory");
        let name = archive_file_name(false);
        let expected = Path::new(name);
        let hashed_name = format!(
            "{}-testhash.{}",
            expected
                .file_stem()
                .expect("runtime archive has a file stem")
                .to_string_lossy(),
            expected
                .extension()
                .expect("runtime archive has an extension")
                .to_string_lossy()
        );
        let hashed = dependencies.join(hashed_name);
        std::fs::write(&hashed, b"archive").expect("hashed runtime archive");

        assert_eq!(find_runtime_archive(&directory, name), Some(hashed.clone()));
        std::fs::remove_dir_all(directory).expect("remove runtime archive test directory");
    }
}
