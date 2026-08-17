//! Why a link, an archive, or a symbol read failed.
//!
//! Every variant names a file or a tool, because the failures this covers are
//! the ones where a linker's own diagnostic names neither: an archive that is
//! not there, a driver that is not in the bundle, a runtime built against a
//! different ABI than the compiler asking for it.

use std::path::PathBuf;

/// Why linking failed.
#[derive(Debug, thiserror::Error)]
pub enum LinkError {
    /// The discovered LLVM install has no `clang` driver.
    #[error("no `clang` linker driver at `{path}` in the discovered LLVM install")]
    DriverMissing {
        /// Where `clang` was expected.
        path: PathBuf,
    },
    /// The discovered LLVM install has no `llvm-ar` archiver.
    #[error("no `llvm-ar` archiver at `{path}` in the discovered LLVM install")]
    ArchiverMissing {
        /// Where `llvm-ar` was expected.
        path: PathBuf,
    },
    /// The discovered LLVM install has no `llvm-nm` symbol reader.
    #[error("no `llvm-nm` symbol reader at `{path}` in the discovered LLVM install")]
    SymbolReaderMissing {
        /// Where `llvm-nm` was expected.
        path: PathBuf,
    },
    /// The symbol reader could not be run.
    #[error("cannot run the symbol reader `{tool}`: {source}")]
    SymbolReaderUnusable {
        /// The symbol reader that could not be spawned.
        tool: PathBuf,
        /// The underlying failure.
        #[source]
        source: std::io::Error,
    },
    /// The symbol reader rejected a selected archive.
    #[error("cannot inspect the foreign archive `{archive}`:\n{diagnostic}")]
    SymbolReaderFailed {
        /// The archive that could not be inspected.
        archive: PathBuf,
        /// The reader's diagnostic.
        diagnostic: String,
    },
    /// The archive's directory could not be prepared, or a stale archive could
    /// not be removed.
    #[error("cannot write the static archive `{path}`: {source}")]
    ArchiveUnwritable {
        /// The archive that could not be written.
        path: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
    /// The archiver reported success and wrote nothing.
    ///
    /// Worth its own name: a consumer that linked against a path with no file
    /// there gets a diagnostic about a missing library rather than about the
    /// build step that was supposed to produce it.
    #[error("the archiver reported success but wrote no archive at `{path}`")]
    ArchiveMissing {
        /// Where the archive was expected.
        path: PathBuf,
    },
    /// The native runtime archive is missing.
    #[error(
        "the native runtime archive `{path}` is missing; build it with \
         `cargo build --workspace`"
    )]
    RuntimeArchiveMissing {
        /// Where the archive was expected.
        path: PathBuf,
    },
    /// The discovered LLVM install ships no `lld`, which a cross link needs.
    ///
    /// Its own failure rather than a linker diagnostic, because the linker
    /// diagnostic is the worst one in the build: clang picks a linker rather
    /// than containing one, so with no `lld` beside it the driver searches
    /// `PATH`, finds the host's, and hands it an object for another machine. A
    /// PE linker given an ELF object says `unrecognised emulation mode:
    /// elf_x86_64`, which names no file, no target, and nothing to do.
    #[error(
        "the discovered LLVM install at `{bin_dir}` ships no `lld`, so it cannot \
         link for `{target}`\n\
         note: clang selects a linker rather than containing one, and the only \
         linker on this machine builds for this machine\n\
         note: install a bundle built with lld (`knvm install-llvm --force`); \
         bundles published before `--target` existed carry clang alone"
    )]
    CrossLinkerMissing {
        /// The bundle's `bin` directory, where `lld` was looked for.
        bin_dir: PathBuf,
        /// The target that was being linked for.
        target: String,
    },
    /// The sysroot a cross link was told to use is not a directory.
    ///
    /// Named where it was configured rather than left to the driver, which
    /// reports a missing `stdio.h` or an unresolvable `-lc` and says nothing
    /// about the setting that sent it looking in a tree that is not there.
    #[error(
        "the sysroot `{path}` for target `{target}` is not a directory\n\
         note: it comes from {source_of_setting}; a cross build needs the target \
         machine's headers and libraries, which the host's toolchain does not have"
    )]
    SysrootMissing {
        /// The path that was going to be passed as `--sysroot`.
        path: PathBuf,
        /// The target it was to be used for.
        target: String,
        /// Where the setting came from, named so it can be corrected.
        source_of_setting: &'static str,
    },
    /// A selected foreign C static archive is missing.
    ///
    /// Named on its own so a package pointing `nativeLibraries` at an archive
    /// that is not there gets a diagnostic about the missing library file rather
    /// than an opaque linker error about undefined foreign symbols.
    #[error("the foreign native archive `{path}` is missing")]
    ForeignArchiveMissing {
        /// Where the archive was expected.
        path: PathBuf,
    },
    /// The linker driver could not be run at all.
    #[error("cannot run the linker driver `{driver}`: {source}")]
    DriverUnusable {
        /// The driver that could not be spawned.
        driver: PathBuf,
        /// The underlying failure.
        #[source]
        source: std::io::Error,
    },
    /// The runtime archive was built against a different `kira_rt_*` contract.
    ///
    /// Caught by name at link time rather than by corruption at run time: this
    /// is exactly the failure the ABI marker exists to make loud.
    #[error(
        "the native runtime archive `{path}` was built against a different \
         version of the runtime ABI (it does not define `{marker}`); rebuild it \
         with `cargo build -p kira-native-bridge`"
    )]
    RuntimeArchiveStale {
        /// The stale archive.
        path: PathBuf,
        /// The marker this compiler expected it to define.
        marker: &'static str,
    },
    /// A file the finished program must find beside itself could not be put
    /// there.
    ///
    /// Named on its own because the failure it prevents is silent: a program
    /// that links clean and cannot start reports whatever the operating system's
    /// loader says, which on Windows is a status code and no file name at all.
    #[error("cannot place the runtime file `{source_path}` beside `{output}`: {source}")]
    RuntimeFileUnplaceable {
        /// The declared file that could not be copied.
        source_path: PathBuf,
        /// What it was to sit beside.
        output: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
    /// The linker ran and rejected the link.
    #[error("linking `{output}` failed:\n{diagnostic}")]
    Failed {
        /// The executable being linked.
        output: PathBuf,
        /// The driver's diagnostics, from both of its output streams.
        diagnostic: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_foreign_archive_names_the_missing_file() {
        let error = LinkError::ForeignArchiveMissing {
            path: PathBuf::from("/nowhere/libffimath.a"),
        };
        assert!(error.to_string().contains("libffimath.a"));
    }

    #[test]
    fn a_missing_runtime_archive_names_how_to_build_it() {
        let error = LinkError::RuntimeArchiveMissing {
            path: PathBuf::from("/nowhere/libkira_native_bridge.a"),
        };
        let text = error.to_string();
        assert!(text.contains("libkira_native_bridge.a"));
        assert!(text.contains("cargo build --workspace"));
    }

    #[test]
    fn a_missing_archiver_names_the_tool_and_where_it_was_looked_for() {
        let error = LinkError::ArchiverMissing {
            path: PathBuf::from("/llvm/bin/llvm-ar"),
        };
        let text = error.to_string();
        assert!(text.contains("llvm-ar"), "{text}");
        assert!(text.contains("/llvm/bin/llvm-ar"), "{text}");
    }

    /// A sysroot failure has to say where the setting came from, because the
    /// path itself is the one thing the reader already knows is wrong.
    #[test]
    fn a_missing_sysroot_names_the_setting_that_chose_it() {
        let error = LinkError::SysrootMissing {
            path: PathBuf::from("/usr/aarch64-linux-gnu"),
            target: "aarch64-linux-gnu".to_owned(),
            source_of_setting: "the `KIRA_SYSROOT` environment variable",
        };
        let text = error.to_string();
        assert!(text.contains("/usr/aarch64-linux-gnu"), "{text}");
        assert!(text.contains("aarch64-linux-gnu"), "{text}");
        assert!(text.contains("KIRA_SYSROOT"), "{text}");
    }
}
