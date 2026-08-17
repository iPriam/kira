//! The thin shared carrier that holds opted-in static FFI archives open.
//!
//! Host-only by construction, and not a target selection at all: the carrier
//! exists so *this* process can `dlopen` it and call the symbols a static
//! archive defines, so a carrier built for another machine is one nothing here
//! could load. That is why nothing in this module takes a
//! [`NativeBuildTarget`](super::target::NativeBuildTarget) other than the host.

use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

use kira_native_lib_definition::NativeLinkInputs;
use kira_toolchain::LlvmInstallation;

use super::LinkError;
use super::driver::{
    force_symbols, platform_link_arguments, reproducible_link_arguments, response_file_for,
    shared_library_flag, stage_runtime_files, tool_diagnostic,
};
use super::target::{NativeBuildTarget, macos_sysroot};

/// Links a symbol-retaining shared carrier for explicitly opted-in static FFI
/// archives.
///
/// The carrier contains archive members selected by the imported symbol names;
/// it has no generated call wrappers or signature-specific code. Libffi calls
/// the exported symbols after the carrier is loaded.
pub fn link_ffi_carrier(
    llvm: &LlvmInstallation,
    foreign_link: &NativeLinkInputs,
    symbols: &[String],
    carrier: &Path,
) -> Result<Vec<String>, LinkError> {
    let target = NativeBuildTarget::host();
    let driver = llvm.clang();
    if !driver.is_file() {
        return Err(LinkError::DriverMissing { path: driver });
    }
    if foreign_link.static_archives().is_empty() {
        return Err(LinkError::Failed {
            output: carrier.to_path_buf(),
            diagnostic: "a thin FFI carrier requires at least one static archive".to_owned(),
        });
    }
    for (_, archive) in foreign_link.static_archives() {
        if !archive.is_file() {
            return Err(LinkError::ForeignArchiveMissing {
                path: archive.clone(),
            });
        }
    }

    // A package's static row can contain declarations satisfied by the host
    // process rather than by the archive itself. Inspect the archive before
    // asking the linker to retain anything: forcing all declarations turns a
    // process binding into a carrier export and makes Windows link.exe report
    // a false unresolved carrier symbol.
    let retained = archive_defined_symbols(llvm, foreign_link, symbols)?;
    if retained.is_empty() {
        return Ok(Vec::new());
    }

    let mut arguments: Vec<std::ffi::OsString> = vec![shared_library_flag(&target).into()];
    arguments.extend(
        force_symbols(&target, retained.iter().cloned())
            .into_iter()
            .map(Into::into),
    );
    for (_, archive) in foreign_link.static_archives() {
        arguments.push(archive.into());
    }
    arguments.push("-o".into());
    arguments.push(carrier.into());
    for argument in foreign_link.driver_arguments() {
        arguments.push(argument.into());
    }
    for argument in platform_link_arguments(&target) {
        arguments.push(argument.into());
    }
    for argument in reproducible_link_arguments(&target) {
        arguments.push(argument.into());
    }
    if let Some(sysroot) = macos_sysroot(&target) {
        arguments.push("-isysroot".into());
        arguments.push(sysroot.into());
    }

    stage_runtime_files(foreign_link, carrier)?;
    let mut command = Command::new(&driver);
    match response_file_for(&arguments, carrier)? {
        Some(response) => {
            let mut flag = std::ffi::OsString::from("@");
            flag.push(response);
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
        return Err(LinkError::Failed {
            output: carrier.to_path_buf(),
            diagnostic: tool_diagnostic(&output),
        });
    }
    Ok(retained)
}

/// Returns requested symbols that are actually defined by one selected static
/// archive, in the request order and without duplicates.
fn archive_defined_symbols(
    llvm: &LlvmInstallation,
    foreign_link: &NativeLinkInputs,
    requested: &[String],
) -> Result<Vec<String>, LinkError> {
    let reader = llvm
        .bin_dir
        .join(kira_toolchain::executable_name("llvm-nm"));
    if !reader.is_file() {
        return Err(LinkError::SymbolReaderMissing { path: reader });
    }
    let requested_set: HashSet<&str> = requested.iter().map(String::as_str).collect();
    let mut defined = HashSet::new();
    for (_, archive) in foreign_link.static_archives() {
        let output = Command::new(&reader)
            .args(["-P", "--extern-only", "--defined-only"])
            .arg(archive)
            .output()
            .map_err(|source| LinkError::SymbolReaderUnusable {
                tool: reader.clone(),
                source,
            })?;
        if !output.status.success() {
            return Err(LinkError::SymbolReaderFailed {
                archive: archive.clone(),
                diagnostic: tool_diagnostic(&output),
            });
        }
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            let Some(name) = line.split_whitespace().next() else {
                continue;
            };
            if name.ends_with(':') {
                continue;
            }
            let name = archive_symbol_name(name);
            if requested_set.contains(name.as_str()) {
                defined.insert(name);
            }
        }
    }
    Ok(requested
        .iter()
        .filter(|symbol| defined.contains(symbol.as_str()))
        .cloned()
        .collect())
}

/// Normalizes the one object-file symbol prefix used by Mach-O `nm` output.
///
/// Reads this host's format rather than a target's, because the archives being
/// inspected are the ones the carrier will link and the carrier is this
/// machine's.
fn archive_symbol_name(name: &str) -> String {
    if cfg!(target_os = "macos") {
        name.strip_prefix('_').unwrap_or(name).to_owned()
    } else {
        name.to_owned()
    }
}
