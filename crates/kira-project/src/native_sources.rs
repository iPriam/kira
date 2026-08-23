//! Building a native library's declared C sources into the archive it links.
//!
//! A `NativeLibrary` declaration says how to build the library (`sources`,
//! `Headers { includeDirs, defines }`), and may also say where the built archive
//! lives per target (`NativeTarget { staticLib }`). Until this module existed
//! only the second half was ever read — [`super::native_libraries`] located the
//! archive on disk and linked whatever it found, with no relationship to the C it
//! was supposedly built from.
//!
//! A declaration that names no archive is complete: `built_archive_path` says
//! where Kira puts one it builds itself.
//!
//! That gap is quiet and it bites hard. Editing a `.c` file changes nothing: the
//! stale archive still links, so either a symbol you just wrote comes back
//! undefined at link time, or — worse, because nothing fails — the old code runs
//! and the edit appears to have been ignored. Every project working this way grows
//! a side-channel script to rebuild archives by hand, which is the same
//! declaration written a third time and free to drift from the other two.
//!
//! So the archive is now built from the sources when it is older than them, by the
//! thing that already knows what the sources are.
//!
//! # Only for the target being built
//!
//! Compiling C for a target needs that target's toolchain, and a checkout building
//! for its own machine has exactly one. A stale archive for some *other* declared
//! target is therefore left alone rather than guessed at — it is not this build's
//! business, and producing a wrong archive is worse than leaving an honest one that
//! fails loudly when someone does build for it.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

use kira_native_lib_definition::{NativeLibrarySpec, TargetTriple};
use kira_toolchain::llvm_discovery::LlvmInstallation;

/// Why a library's sources could not be built into its archive.
#[derive(Debug, thiserror::Error)]
pub enum NativeSourceBuildError {
    /// A declared source, header directory, or output path could not be used.
    #[error("native library `{library}`: cannot access `{path}`: {message}")]
    Io {
        /// The library being built.
        library: String,
        /// The path that could not be used.
        path: String,
        /// The underlying I/O failure, rendered.
        message: String,
    },
    /// The C compiler could not be run at all.
    #[error("native library `{library}`: cannot run `{compiler}`: {message}")]
    CompilerUnavailable {
        /// The library being built.
        library: String,
        /// The compiler that could not be spawned.
        compiler: String,
        /// The underlying failure, rendered.
        message: String,
    },
    /// The C compiler ran and rejected a source.
    #[error("native library `{library}`: compiling `{file}` failed\n{output}")]
    CompileFailed {
        /// The library being built.
        library: String,
        /// The source that did not compile. Not named `source`: thiserror reads
        /// that name as the error's cause and would try to chain a String.
        file: String,
        /// What the compiler said.
        output: String,
    },
    /// The archiver ran and failed.
    #[error("native library `{library}`: archiving `{archive}` failed\n{output}")]
    ArchiveFailed {
        /// The library being built.
        library: String,
        /// The archive that could not be written.
        archive: String,
        /// What the archiver said.
        output: String,
    },
}

/// Builds `spec`'s sources into the archive its `target` row names, if that
/// archive is missing or older than any input.
///
/// A no-op for a library that declares no sources (one shipping a prebuilt
/// archive), for a target row naming no archive, and for a target this host
/// cannot compile for.
pub fn ensure_archive_current(
    spec: &NativeLibrarySpec,
    base_dir: &Path,
    target: &TargetTriple,
) -> Result<(), NativeSourceBuildError> {
    if spec.sources().is_empty() {
        return Ok(());
    }
    if !builds_natively(target) {
        return Ok(());
    }
    let Some(row) = spec.targets().iter().find(|row| row.triple() == target) else {
        return Ok(());
    };
    let Some(relative) = spec.archive_path(row) else {
        return Ok(());
    };
    let archive = base_dir.join(relative);

    let sources: Vec<PathBuf> = spec
        .sources()
        .iter()
        .map(|source| base_dir.join(source))
        .collect();
    if !is_stale(&archive, spec, base_dir, &sources) {
        return Ok(());
    }

    let windows = target.os() == "windows";
    let installation = kira_toolchain::llvm_discovery::discover(None).ok();
    let compiler = tool(
        "CC",
        installation.as_ref().map(LlvmInstallation::clang),
        fallback_compiler(windows),
    );
    let mut flags: Vec<String> = vec!["-O2".into()];
    // Position-independent code is what a PE image does anyway, and there is no
    // flag for asking: clang targeting Windows answers `-fPIC` with "argument
    // unused during compilation", which is noise on every source of every build.
    if windows {
        // The UCRT marks `sscanf`, `strcpy` and most of <string.h> deprecated,
        // advising the `_s` variants — which are Microsoft's alone. Portable C
        // cannot take that advice, so the warning fires on every source of
        // every library and says nothing actionable. Silencing it is what the
        // define exists for.
        flags.push("-D_CRT_SECURE_NO_WARNINGS".into());
        // And the same again for the POSIX names it deprecates in favour of an
        // underscored spelling — `strdup` for `_strdup`. Those are the portable
        // ones; taking the advice is what would make the source non-portable.
        flags.push("-D_CRT_NONSTDC_NO_WARNINGS".into());
    } else {
        flags.push("-fPIC".into());
    }
    // The managed LLVM clang ships no default sysroot, so on Apple targets it
    // finds neither the frameworks (`<CoreFoundation/CoreFoundation.h>`) nor the
    // C library headers unless `-isysroot` names the active SDK. Apple's own `cc`
    // embeds that path; a portable LLVM clang has to be told it.
    if matches!(target.os(), "macos" | "ios" | "tvos" | "xros")
        && let Some(sdk) = apple_sdk_root()
    {
        flags.push("-isysroot".into());
        flags.push(sdk);
    }
    if let Some(headers) = spec.headers() {
        for directory in &headers.include_dirs {
            flags.push("-I".into());
            flags.push(compiler_path(&base_dir.join(directory)));
        }
        for define in &headers.defines {
            flags.push(format!("-D{define}"));
        }
    }
    for define in row.defines() {
        flags.push(format!("-D{define}"));
    }

    // Objects live beside the archive, prefixed so a directory listing says what
    // they are and which target they are for.
    let objects_dir = archive
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!(".{}-{}-objects", spec.name(), target));
    create_dir(spec.name(), &objects_dir)?;
    if let Some(parent) = archive.parent() {
        create_dir(spec.name(), parent)?;
    }

    let mut objects = Vec::with_capacity(sources.len());
    for (index, source) in sources.iter().enumerate() {
        let stem = source
            .file_stem()
            .map(|stem| stem.to_string_lossy().to_string())
            .unwrap_or_else(|| "source".to_string());
        let object = objects_dir.join(format!("{index:04}-{stem}.o"));
        let output = Command::new(&compiler)
            .args(&flags)
            .args(["-x", source_language(source, target)])
            .arg("-c")
            .arg(source)
            .arg("-o")
            .arg(&object)
            .output()
            .map_err(|error| NativeSourceBuildError::CompilerUnavailable {
                library: spec.name().to_string(),
                compiler: compiler.display().to_string(),
                message: error.to_string(),
            })?;
        if !output.status.success() {
            return Err(NativeSourceBuildError::CompileFailed {
                library: spec.name().to_string(),
                file: source.display().to_string(),
                output: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }
        objects.push(object);
    }

    // Written from scratch: `ar r` into a surviving archive would keep objects
    // for sources the manifest no longer lists.
    let _ = std::fs::remove_file(&archive);
    let archiver = tool(
        "AR",
        installation.as_ref().map(LlvmInstallation::llvm_ar),
        fallback_archiver(windows),
    );
    let output = Command::new(&archiver)
        .arg("rcs")
        .arg(&archive)
        .args(&objects)
        .output()
        .map_err(|error| NativeSourceBuildError::CompilerUnavailable {
            library: spec.name().to_string(),
            compiler: archiver.display().to_string(),
            message: error.to_string(),
        })?;
    if !output.status.success() {
        return Err(NativeSourceBuildError::ArchiveFailed {
            library: spec.name().to_string(),
            archive: archive.display().to_string(),
            output: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    Ok(())
}

/// A path in the form a compiler will accept.
///
/// Windows canonicalization answers with an extended-length path
/// (`\\?\C:\...`). The OS opens one of those without complaint, which is why
/// a source file named that way compiles — but clang's *header search* does not
/// resolve them, so an `-I` handed one contributes nothing and every angle
/// include below it goes missing. The first Windows build of this workspace hit
/// exactly that: freetype compiled its own sources and could not find
/// `<freetype/internal/ftdebug.h>` in the include directory the manifest
/// declares.
///
/// Stripping the prefix costs the >260-character path support it exists for.
/// That is the right trade here: the compiler cannot use those paths anyway, so
/// the choice is a working build with ordinary paths or a broken one with long
/// ones.
pub(crate) fn compiler_path(path: &Path) -> String {
    let text = path.display().to_string();
    // `\\?\UNC\server\share` is the verbatim spelling of `\\server\share`.
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    text.strip_prefix(r"\\?\")
        .map_or_else(|| text.clone(), ToOwned::to_owned)
}

/// Which binary to run for a tool: what the environment names, else the one in
/// the managed LLVM install, else a bare name off `PATH`.
///
/// The environment wins because a cross build or a distribution package has to
/// be able to say. The managed install comes next for the reason
/// [`LlvmInstallation::llvm_ar`] gives about itself — a toolchain that picks its
/// tools off `PATH` works on one machine — and it is the install this crate
/// already reads clang out of for header autobinding, so the C a package
/// compiles and the C its bindings are parsed with come from one place.
fn tool(variable: &str, managed: Option<PathBuf>, fallback: &str) -> PathBuf {
    if let Ok(named) = std::env::var(variable)
        && !named.is_empty()
    {
        return PathBuf::from(named);
    }
    managed
        .filter(|path| path.exists())
        .unwrap_or_else(|| PathBuf::from(fallback))
}

/// The active Apple SDK's root, as `xcrun` reports it.
///
/// `xcrun --show-sdk-path` is the supported way to ask which SDK `xcode-select`
/// has active. Returns `None` when `xcrun` is absent or fails, in which case the
/// compile falls back to whatever default the compiler carries — correct for
/// Apple's `cc`, and no worse than before for a managed clang.
fn apple_sdk_root() -> Option<String> {
    let output = Command::new("xcrun")
        .args(["--show-sdk-path"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if path.is_empty() { None } else { Some(path) }
}

/// The compiler to look for on `PATH` when no install was discovered.
///
/// `cc` is a Unix convention — a name every POSIX host has, usually a symlink to
/// whichever compiler it installed. Windows has no such name, so a native-source
/// build there failed with "program not found" and named a compiler the machine
/// had never been asked to have. `clang` is the name to try instead: it is what
/// the managed toolchain installs, and `clang.exe` drives the GNU-style command
/// line this passes (`-c`, `-o`, `-I`, `-x`), unlike `clang-cl.exe`, which wants
/// MSVC spellings for every one of them.
fn fallback_compiler(windows: bool) -> &'static str {
    if windows { "clang" } else { "cc" }
}

/// The archiver to look for on `PATH` when no install was discovered.
///
/// Same shape: `ar` is a name Windows does not have, and `llvm-ar` takes the
/// same `rcs` arguments this passes.
fn fallback_archiver(windows: bool) -> &'static str {
    if windows { "llvm-ar" } else { "ar" }
}

/// The language clang compiles one declared source as.
///
/// Stated per source rather than left to the suffix, for one reason on each
/// side of the split. A framework-backed implementation keeps a cross-platform
/// `.c` filename and hides Objective-C behind `__APPLE__` — Sokol does — so on
/// Apple targets C sources compile as Objective-C, which is a strict superset
/// of the C they otherwise are. And a library that ships C++ ships it as `.cc`
/// — HarfBuzz does — so a blanket Objective-C mode compiled it as the wrong
/// language and every `<cassert>` it includes went missing.
fn source_language(source: &Path, target: &TargetTriple) -> &'static str {
    let apple = matches!(target.os(), "macos" | "ios" | "tvos" | "xros");
    let suffix = source
        .extension()
        .and_then(|suffix| suffix.to_str())
        .unwrap_or_default();
    match (suffix, apple) {
        ("cc" | "cpp" | "cxx" | "mm", true) => "objective-c++",
        ("cc" | "cpp" | "cxx" | "mm", false) => "c++",
        ("m", _) => "objective-c",
        (_, true) => "objective-c",
        (_, false) => "c",
    }
}

fn create_dir(library: &str, path: &Path) -> Result<(), NativeSourceBuildError> {
    std::fs::create_dir_all(path).map_err(|error| NativeSourceBuildError::Io {
        library: library.to_string(),
        path: path.display().to_string(),
        message: error.to_string(),
    })
}

/// Whether this host can compile for `target` with its own C compiler.
///
/// Deliberately narrow: the triple has to be the machine we are on. Anything else
/// wants a cross toolchain this cannot assume, and a wrong archive is worse than
/// an untouched one.
fn builds_natively(target: &TargetTriple) -> bool {
    let text = target.to_string();
    // A Kira triple spells its architecture and operating system the way Rust's
    // own constants do — `aarch64`, `x86_64`, `macos`, `linux`, `windows` — so
    // the host's are the strings to match against directly.
    text.starts_with(std::env::consts::ARCH) && text.contains(std::env::consts::OS)
}

/// Whether the archive needs rebuilding: absent, or older than any source or any
/// header it is compiled against.
///
/// Header timestamps matter as much as source ones — a signature change in a `.h`
/// with no `.c` edit is exactly the case where a stale archive links and misleads.
/// Only headers in the declared include directories are considered, one level
/// deep: vendored trees hold thousands of files and walking them all every build
/// would cost more than the compile it is trying to avoid.
fn is_stale(
    archive: &Path,
    spec: &NativeLibrarySpec,
    base_dir: &Path,
    sources: &[PathBuf],
) -> bool {
    let Some(built) = modified(archive) else {
        return true;
    };
    for source in sources {
        match modified(source) {
            Some(stamp) if stamp <= built => {}
            // A source that cannot be read is left to the compiler to complain
            // about, with its own error message rather than a guess from here.
            None => return true,
            _ => return true,
        }
    }
    if let Some(headers) = spec.headers() {
        if let Some(entrypoint) = &headers.entrypoint
            && modified(&base_dir.join(entrypoint)).is_none_or(|stamp| stamp > built)
        {
            return true;
        }
        for directory in &headers.include_dirs {
            let Ok(entries) = std::fs::read_dir(base_dir.join(directory)) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|extension| extension == "h")
                    && modified(&path).is_some_and(|stamp| stamp > built)
                {
                    return true;
                }
            }
        }
    }
    false
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The split that decides whether a declared source compiles at all: a
    /// framework-backed `.c` needs Objective-C on Apple, and a C++ source needs
    /// C++ everywhere — one blanket mode gets one of them wrong.
    #[test]
    fn each_source_compiles_as_the_language_it_is_written_in() {
        let apple = TargetTriple::new("aarch64", "macos", "none");
        let linux = TargetTriple::new("x86_64", "linux", "gnu");

        assert_eq!(
            source_language(Path::new("sokol_impl.c"), &apple),
            "objective-c"
        );
        assert_eq!(source_language(Path::new("sokol_impl.c"), &linux), "c");
        assert_eq!(
            source_language(Path::new("harfbuzz.cc"), &apple),
            "objective-c++"
        );
        assert_eq!(source_language(Path::new("harfbuzz.cc"), &linux), "c++");
        assert_eq!(
            source_language(Path::new("shim.mm"), &apple),
            "objective-c++"
        );
        assert_eq!(source_language(Path::new("shim.m"), &linux), "objective-c");
    }

    #[test]
    fn windows_names_the_compiler_and_archiver_that_host_actually_has() {
        // `cc` and `ar` are Unix conventions. Naming them on Windows fails with
        // "program not found", which reads as a broken install rather than as a
        // toolchain this never had.
        assert_eq!(fallback_compiler(true), "clang");
        assert_eq!(fallback_archiver(true), "llvm-ar");
        assert_eq!(fallback_compiler(false), "cc");
        assert_eq!(fallback_archiver(false), "ar");
    }

    #[test]
    fn a_managed_tool_beats_the_bare_name_but_the_environment_beats_both() {
        let managed = std::env::current_exe().expect("this test binary exists");
        // Nothing names the variable: the discovered install wins over `PATH`.
        assert_eq!(
            tool("KIRA_TEST_TOOL_UNSET", Some(managed.clone()), "cc"),
            managed
        );
        // A discovered path that is not actually there is not worth running.
        assert_eq!(
            tool(
                "KIRA_TEST_TOOL_UNSET",
                Some(PathBuf::from("/nowhere/clang")),
                "cc"
            ),
            PathBuf::from("cc")
        );
        // No install at all falls back to the bare name for the host.
        assert_eq!(
            tool("KIRA_TEST_TOOL_UNSET", None, fallback_compiler(true)),
            PathBuf::from("clang")
        );
    }

    #[test]
    fn an_include_path_loses_the_prefix_a_header_search_cannot_read() {
        // The break this exists for: clang opened the source fine and then
        // could not find a header in the directory `-I` named.
        assert_eq!(
            compiler_path(Path::new(r"\\?\C:\Users\x\vendor\freetype\include")),
            r"C:\Users\x\vendor\freetype\include"
        );
        // A share keeps its two leading slashes: `\\?\UNC\srv\share` IS
        // `\\srv\share`, so dropping the prefix outright would name a
        // different path rather than the same one spelled plainly.
        assert_eq!(
            compiler_path(Path::new(r"\\?\UNC\srv\share\include")),
            r"\\srv\share\include"
        );
        // Everything else is handed through untouched, which is every path on
        // every other platform.
        assert_eq!(
            compiler_path(Path::new("/usr/local/include")),
            "/usr/local/include"
        );
    }
}
