//! Linking native executables and shared libraries.
//!
//! Codegen happens in process through LLVM; `clang` is used only here, as the
//! linker driver, and always the `clang` from the discovered LLVM install
//! rather than whatever is on `PATH` — the same explicit-toolchain rule the
//! rest of the backend follows. That one driver links for every machine Kira
//! emits for: clang is a cross compiler by construction, so aiming it at
//! another target is `--target=` plus a `--sysroot` holding that machine's
//! libraries, not a second toolchain.
//!
//! The link inputs are the program's object files and the native runtime archive
//! (`libkira_native_bridge.a`), which is a Rust `staticlib` and therefore carries
//! the Rust standard library with it; the driver supplies the system libraries
//! around it. A cross link needs that archive built for the target as well —
//! the Rust standard library inside it is machine code like any other — which
//! is why the caller resolves it per target rather than naming one file.
//!
//! # Layout
//!
//! - [`error`] — every way a link, an archive, or a symbol read can fail.
//! - [`target`] — which machine is being linked for, and its sysroot.
//! - [`driver`] — the flags, the response file, and the staged runtime files.
//! - [`libraries`] — the shared, live, hybrid, and static-archive artifacts.
//! - [`carrier`] — the host-only shared carrier for static FFI archives.
//!
//! What stays here is the executable link and [`link_with`], the one function
//! that actually spawns the driver.

use std::path::{Path, PathBuf};
use std::process::Command;

use kira_native_lib_definition::NativeLinkInputs;
use kira_toolchain::LlvmInstallation;

mod carrier;
mod driver;
mod error;
mod libraries;
mod target;

pub use carrier::link_ffi_carrier;
pub use error::LinkError;
pub use libraries::{
    archive_static_library, link_hybrid_library, link_hybrid_library_debug,
    link_native_live_library, link_shared_library,
};
pub use target::{NativeBuildTarget, SYSROOT_VARIABLE};

use driver::{
    executable_stack_arguments, export_debug_symbols, platform_link_arguments,
    reproducible_link_arguments, response_file_for, stage_runtime_files, tool_diagnostic,
};
use target::macos_sysroot;

/// Links `objects` against the native runtime archive into `executable`.
///
/// `foreign_link` are the resolved C link inputs that satisfy the program's
/// `@FFI.Extern` imports. Each generated adapter references its C symbol, so
/// naming the archives on the link line is enough for the linker to pull in
/// exactly the members those symbols need — no force-loading, and a missing
/// symbol becomes a named link error rather than a silent empty binary. The
/// frameworks, system libraries, and linker flags declared beside those
/// archives follow them on the line, because a library may supply its symbols
/// through those alone.
///
/// `objects` are the program's codegen units, in unit order. A program emitted
/// in one unit has one; a program split across several has one per unit, and
/// every cross-unit call is an ordinary undefined symbol the linker resolves.
///
/// `target` is the machine being linked for, and it decides more than the
/// `--target=` flag: the platform library list, the symbol-forcing spelling, the
/// stack reserve, and whether the image is a PIE all follow it rather than
/// following this host.
pub fn link_executable(
    llvm: &LlvmInstallation,
    objects: &[PathBuf],
    runtime_archive: &Path,
    foreign_link: &NativeLinkInputs,
    executable: &Path,
    target: &NativeBuildTarget,
) -> Result<(), LinkError> {
    let extra = executable_stack_arguments(target);
    link_executable_inner(
        llvm,
        objects,
        runtime_archive,
        foreign_link,
        executable,
        extra,
        target,
    )
}

/// Links a native executable and asks the platform linker to retain a debugger
/// symbol file alongside the executable.
pub fn link_executable_debug(
    llvm: &LlvmInstallation,
    objects: &[PathBuf],
    runtime_archive: &Path,
    foreign_link: &NativeLinkInputs,
    executable: &Path,
    debug_symbols: &[String],
    target: &NativeBuildTarget,
) -> Result<(), LinkError> {
    let mut extra = executable_stack_arguments(target);
    // The native object carries portable DWARF plus the platform-native debug
    // records where the target needs them. `-g` asks the linker for its native
    // symbol companion (a PDB on Windows), while the object stays beside the
    // artifact for direct DWARF inspection.
    extra.push("-g".to_owned());
    extra.extend(export_debug_symbols(target, debug_symbols));
    link_executable_inner(
        llvm,
        objects,
        runtime_archive,
        foreign_link,
        executable,
        extra,
        target,
    )
}

fn link_executable_inner(
    llvm: &LlvmInstallation,
    objects: &[PathBuf],
    runtime_archive: &Path,
    foreign_link: &NativeLinkInputs,
    executable: &Path,
    extra: Vec<String>,
    target: &NativeBuildTarget,
) -> Result<(), LinkError> {
    link_with(
        llvm,
        objects,
        runtime_archive,
        foreign_link,
        executable,
        &extra,
        target,
    )
}

/// Whether the discovered bundle carries the linker a cross link is aimed at.
///
/// `ld.lld` is the name clang resolves `-fuse-ld=lld` to on ELF and Mach-O
/// hosts, `lld-link` the one it resolves to for PE; a bundle built with the
/// `lld` project installs all of them, so finding either says the project is
/// there. Asked of the bundle's own `bin` directory and never of `PATH`, for
/// the same reason the driver and the archiver are: the toolchain a build uses
/// is the managed one, not whatever the machine happens to have.
fn ships_lld(llvm: &LlvmInstallation) -> bool {
    ["ld.lld", "lld-link", "lld"].iter().any(|name| {
        llvm.bin_dir
            .join(kira_toolchain::executable_name(name))
            .is_file()
    })
}

/// Runs the linker driver over `objects` plus the runtime archive and any
/// selected foreign C archives.
pub(crate) fn link_with(
    llvm: &LlvmInstallation,
    objects: &[PathBuf],
    runtime_archive: &Path,
    foreign_link: &NativeLinkInputs,
    output: &Path,
    extra: &[String],
    target: &NativeBuildTarget,
) -> Result<(), LinkError> {
    let executable = output;
    let driver = llvm.clang();
    if !driver.is_file() {
        return Err(LinkError::DriverMissing { path: driver });
    }
    if !runtime_archive.is_file() {
        return Err(LinkError::RuntimeArchiveMissing {
            path: runtime_archive.to_path_buf(),
        });
    }
    if !target.is_host() && !ships_lld(llvm) {
        return Err(LinkError::CrossLinkerMissing {
            bin_dir: llvm.bin_dir.clone(),
            target: target.target().to_string(),
        });
    }
    // Checked before the driver runs, where the setting that chose it can still
    // be named. A sysroot that is not there otherwise surfaces as a missing
    // `stdio.h` or an unresolvable `-lc`, neither of which mentions the
    // configuration that sent the driver into a tree with nothing in it.
    if let Some(sysroot) = target.sysroot()
        && !sysroot.is_dir()
    {
        return Err(LinkError::SysrootMissing {
            path: sysroot,
            target: target.target().to_string(),
            source_of_setting: target.sysroot_setting(),
        });
    }
    for archive in foreign_link.archives() {
        if !archive.is_file() {
            return Err(LinkError::ForeignArchiveMissing {
                path: archive.clone(),
            });
        }
    }

    let mut arguments: Vec<std::ffi::OsString> = Vec::new();
    // The target arguments come first so everything after them is interpreted
    // for the machine being built for: clang decides which runtime library and
    // which default library paths apply from `--target`, and a `-l` seen before
    // it is resolved against the host's.
    for argument in target.link_arguments() {
        arguments.push(argument.into());
    }
    for object in objects {
        arguments.push(object.into());
    }
    // The foreign link inputs precede the runtime archive so direct foreign
    // symbol references are satisfied by the library that defines them.
    for archive in foreign_link.archives() {
        arguments.push(archive.into());
    }
    arguments.push(runtime_archive.into());
    arguments.push("-o".into());
    arguments.push(executable.into());
    // The frameworks, system libraries, and linker flags the selected rows
    // declared. They follow the archives for the same reason the archives
    // precede the runtime: a system library resolves symbols left of it.
    for argument in foreign_link.driver_arguments() {
        arguments.push(argument.into());
    }
    for argument in extra {
        arguments.push(argument.into());
    }
    // So a program finds the libraries staged next to it without being told
    // where to look.
    for argument in driver::staged_library_runtime_arguments(target) {
        arguments.push(argument.into());
    }
    for argument in platform_link_arguments(target) {
        arguments.push(argument.into());
    }
    for argument in reproducible_link_arguments(target) {
        arguments.push(argument.into());
    }
    // The managed clang is not Apple's, so it has no built-in knowledge of
    // where the platform libraries live; without an explicit sysroot a host
    // macOS link fails on `library 'System' not found`.
    if let Some(sysroot) = macos_sysroot(target) {
        arguments.push("-isysroot".into());
        arguments.push(sysroot.into());
    }

    stage_runtime_files(foreign_link, executable)?;

    let mut command = Command::new(&driver);
    match response_file_for(&arguments, executable)? {
        Some(response) => {
            let mut flag = std::ffi::OsString::from("@");
            flag.push(&response);
            command.arg(flag);
        }
        None => {
            command.args(&arguments);
        }
    }

    let output = command
        .output()
        .map_err(|source| LinkError::DriverUnusable {
            driver: driver.clone(),
            source,
        })?;
    if !output.status.success() {
        let diagnostic = tool_diagnostic(&output);
        // The ABI marker is the one undefined symbol with a known cause, so say
        // the cause rather than making the reader decode a linker diagnostic.
        if diagnostic.contains(kira_runtime_abi::RUNTIME_ABI_MARKER) {
            return Err(LinkError::RuntimeArchiveStale {
                path: runtime_archive.to_path_buf(),
                marker: kira_runtime_abi::RUNTIME_ABI_MARKER,
            });
        }
        return Err(LinkError::Failed {
            output: executable.to_path_buf(),
            diagnostic,
        });
    }
    Ok(())
}
