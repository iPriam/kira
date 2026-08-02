//! Building a native library's declared C sources into the archive it links.
//!
//! A `NativeLibrary` declaration says two things about the same library: how to
//! build it (`sources`, `Headers { includeDirs, defines }`) and where the built
//! archive lives per target (`NativeTarget { staticLib }`). Until this module
//! existed only the second half was ever read — [`super::native_libraries`]
//! located the archive on disk and linked whatever it found, with no relationship
//! to the C it was supposedly built from.
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
    let Some(relative) = row.artifact().path() else {
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

    let compiler = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let mut flags: Vec<String> = vec!["-O2".into(), "-fPIC".into()];
    // macOS framework-backed implementations (notably Sokol) commonly keep a
    // cross-platform `.c` filename while compiling Objective-C sections behind
    // `__APPLE__`. Clang chooses its language from the suffix unless told
    // otherwise, so an otherwise harmless source timestamp change used to rebuild
    // that file as plain C and fail on AppKit's `@class` declarations. Objective-C
    // is a strict superset for these C sources; keep other targets in C mode.
    if target.to_string().contains("macos") {
        flags.push("-x".into());
        flags.push("objective-c".into());
    }
    if let Some(headers) = spec.headers() {
        for directory in &headers.include_dirs {
            flags.push("-I".into());
            flags.push(base_dir.join(directory).display().to_string());
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
            .arg("-c")
            .arg(source)
            .arg("-o")
            .arg(&object)
            .output()
            .map_err(|error| NativeSourceBuildError::CompilerUnavailable {
                library: spec.name().to_string(),
                compiler: compiler.clone(),
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
    let archiver = std::env::var("AR").unwrap_or_else(|_| "ar".to_string());
    let output = Command::new(&archiver)
        .arg("rcs")
        .arg(&archive)
        .args(&objects)
        .output()
        .map_err(|error| NativeSourceBuildError::CompilerUnavailable {
            library: spec.name().to_string(),
            compiler: archiver.clone(),
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

fn create_dir(library: &str, path: &Path) -> Result<(), NativeSourceBuildError> {
    std::fs::create_dir_all(path).map_err(|error| NativeSourceBuildError::Io {
        library: library.to_string(),
        path: path.display().to_string(),
        message: error.to_string(),
    })
}

/// Whether this host can compile for `target` with its own `cc`.
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
