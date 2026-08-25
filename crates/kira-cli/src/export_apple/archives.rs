//! Where an Apple build's static inputs come from, and what they must be.
//!
//! Three archives meet one Xcode link: the *support* archive carrying
//! `kira_live_runner_entry` and the whole Rust runtime, cross-built per Apple
//! rust target; the *libffi* engine, which exists only for host platforms and
//! is re-stamped for the others; and the *sysroot*, which Xcode's own toolchain
//! answers for. Each lookup fails by naming the exact command that produces
//! the missing file — a build this toolchain can arrange should never fail with
//! "file not found" alone.

use std::path::{Path, PathBuf};
use std::process::Command;

use kira_export::apple::slices;

/// Locates the libffi engine archive linked beside one slice's objects.
///
/// Kira links libffi statically, and the managed install keys installs by
/// *host* platform — there is no `ios-aarch64` entry, because nothing built a
/// separate libffi for the device. The machine code for arm64 libffi is the
/// same across Apple platforms; what differs is the Mach-O *build-version*
/// stamp, and a linker refuses an object stamped for the wrong platform even
/// when every byte of code is right. So the arm64 macOS engine is unpacked,
/// each member is relinked as relocatable output under the slice's own
/// target (which re-stamps it honestly for that SDK), and the result is
/// repacked next to this slice's other artifacts. On macOS itself, where the
/// stamp already agrees, the managed archive is used as-is.
pub(crate) fn libffi_archive_for_slice(
    slice: &slices::ArchSlice,
    work_root: &Path,
) -> Result<PathBuf, String> {
    let source = managed_libffi_archive()?;
    if slice.os == "macos" {
        return Ok(source);
    }
    let dest_directory = work_root.join("libffi");
    let dest = dest_directory.join("libkira_libffi.a");
    if dest.is_file()
        && std::fs::metadata(&dest)
            .and_then(|meta| meta.modified())
            .ok()
            .zip(
                std::fs::metadata(&source)
                    .and_then(|meta| meta.modified())
                    .ok(),
            )
            .is_some_and(|(built, src)| built >= src)
    {
        return Ok(dest);
    }

    let objects = dest_directory.join("objects");
    let _ = std::fs::remove_dir_all(&objects);
    std::fs::create_dir_all(&objects).map_err(|error| error.to_string())?;
    run_tool(
        Command::new("/usr/bin/ar")
            .args(["-x", &source.display().to_string()])
            .current_dir(&objects),
    )?;
    run_tool(
        Command::new("/bin/chmod")
            .arg("-R")
            .arg("u+w")
            .arg(&objects),
    )?;

    let mut members = Vec::new();
    let entries = std::fs::read_dir(&objects).map_err(|error| error.to_string())?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) == Some("o") {
            members.push(path);
        }
    }
    members.sort();

    let triple = format!(
        "{}-apple-{}{}",
        slice.arch,
        slice.os,
        match slice.abi {
            "sim" => "-simulator".to_owned(),
            _ => String::new(),
        }
    );
    let _ = triple;
    let platform = slice.platform_code();
    let mut restamped = Vec::with_capacity(members.len());
    for member in &members {
        // SAFETY-free: plain file reads and writes follow.
        let bytes = std::fs::read(member).map_err(|error| error.to_string())?;
        let patched = restamp_macho_platform(&bytes, platform)
            .map_err(|error| format!("{}: {error}", member.display()))?;
        let out = member.with_extension("restamp.o");
        std::fs::write(&out, patched).map_err(|error| error.to_string())?;
        restamped.push(out);
    }

    let mut command = Command::new("/usr/bin/libtool");
    command.args(["-static", "-o"]).arg(&dest);
    for member in &restamped {
        command.arg(member);
    }
    run_tool(&mut command)?;
    Ok(dest)
}

/// The managed arm64/x86_64 macOS libffi archive this host has installed.
fn managed_libffi_archive() -> Result<PathBuf, String> {
    if let Some(home) = std::env::var_os("KIRA_LIBFFI_HOME") {
        let named = PathBuf::from(home);
        let archive = named
            .join("lib")
            .join(kira_toolchain::static_archive_name_for("macos", ""));
        if archive.is_file() {
            return Ok(archive);
        }
        return Err(format!(
            "KIRA_LIBFFI_HOME names `{}`, which holds no libffi archive",
            named.display()
        ));
    }
    let version = kira_toolchain::libffi_pinned_version().map_err(|error| error.to_string())?;
    let key = format!("macos-{}", kira_export_key_arch());
    let home =
        kira_toolchain::managed_libffi_home(version, &key).map_err(|error| error.to_string())?;
    let archive = home
        .join("lib")
        .join(kira_toolchain::static_archive_name_for("macos", ""));
    if archive.is_file() {
        return Ok(archive);
    }
    Err(format!(
        "no managed libffi for `{key}` at `{}`; install it with `knvm install libffi`",
        archive.display()
    ))
}

/// This machine's architecture in a managed-home key.
fn kira_export_key_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        _ => "aarch64",
    }
}

/// Runs a tool, turning failure into a message naming it.
fn run_tool(command: &mut Command) -> Result<(), String> {
    let status = command
        .status()
        .map_err(|error| format!("`{:?}` could not start: {error}", command.get_program()))?;
    if status.success() {
        return Ok(());
    }
    Err(format!(
        "`{:?}` failed with {status}",
        command.get_program()
    ))
}

/// The 64-bit Mach-O magic, as a little-endian read of the file's first word.
const MH_MAGIC_64: u32 = 0xfeed_facf;
/// `LC_BUILD_VERSION` from `mach-o/loader.h`.
const LC_BUILD_VERSION: u32 = 0x32;
/// Byte offset of the `platform` field inside an `LC_BUILD_VERSION`.
const PLATFORM_OFFSET: usize = 8;

/// Rewrites the platform an object file's `LC_BUILD_VERSION` names.
///
/// The load command is fixed-layout: `cmd`, `cmdsize`, `platform`, then
/// version fields. Only the platform word changes — the code is the machine
/// code it always was, and this records honestly which OS family this build
/// of it is being linked into. An object without the command (or a non-Mach-O
/// file) passes through untouched: there is nothing to correct.
fn restamp_macho_platform(bytes: &[u8], platform: u32) -> Result<Vec<u8>, String> {
    let magic = read_u32(bytes, 0).ok_or("truncated Mach-O header")?;
    if magic != MH_MAGIC_64.swap_bytes() && magic != MH_MAGIC_64 {
        // Not a 64-bit Mach-O; nothing here stamps a platform.
        return Ok(bytes.to_vec());
    }
    let (ncmds, sizeofcmds) = {
        let ncmds = read_u32(bytes, 16).ok_or("truncated Mach-O header")?;
        let sizeofcmds = read_u32(bytes, 20).ok_or("truncated Mach-O header")? as usize;
        (ncmds, sizeofcmds)
    };

    let mut patched = bytes.to_vec();
    let mut offset = 32usize; // sizeof(mach_header_64)
    let end = 32 + sizeofcmds;
    for _ in 0..ncmds {
        if offset + 8 > end || offset + 8 > patched.len() {
            break;
        }
        let cmd = read_u32(&patched, offset).ok_or("truncated load command")?;
        let cmdsize = read_u32(&patched, offset + 4).unwrap_or(0) as usize;
        if cmd == LC_BUILD_VERSION {
            let at = offset + PLATFORM_OFFSET;
            patched[at..at + 4].copy_from_slice(&platform.to_le_bytes());
            return Ok(patched);
        }
        if cmdsize == 0 {
            return Err("a zero-sized load command ends the scan".to_owned());
        }
        offset += cmdsize;
    }
    Ok(patched.to_vec())
}

/// Reads a little-endian `u32`, `None` past the buffer's end.
fn read_u32(bytes: &[u8], at: usize) -> Option<u32> {
    bytes
        .get(at..at + 4)
        .map(|slice| u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

/// Locates the support archive cross-built for `rust_triple`.
///
/// Same search order as the native bridge archive: an explicit environment
/// answer first, then beside this compiler, then the cargo dev-tree layout.
/// Missing is a named command, not a shrug — the archive is one cargo build
/// away.
pub(crate) fn locate_support_archive(rust_triple: &str) -> Result<PathBuf, String> {
    const ARCHIVE: &str = "libkira_app_runner.a";
    let variable = format!(
        "KIRA_APP_RUNNER_{}",
        rust_triple.replace('-', "_").to_uppercase()
    );
    if let Some(named) = std::env::var_os(&variable) {
        let path = PathBuf::from(named);
        if path.is_file() {
            return Ok(path);
        }
        return Err(format!(
            "{variable} names `{}`, which does not exist",
            path.display()
        ));
    }

    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let bin = executable.parent().ok_or("no executable directory")?;
    let mut searched = vec![bin.join(rust_triple).join(ARCHIVE)];
    if let (Some(parent), Some(profile)) = (bin.parent(), bin.file_name()) {
        searched.push(parent.join(rust_triple).join(profile).join(ARCHIVE));
    }
    if let Some(found) = searched.iter().find(|path| path.is_file()) {
        return Ok(found.clone());
    }
    Err(format!(
        "no application runner archive for `{rust_triple}` was found (looked in {}); \
         build it with `cargo build -p kira-app-runner --target {rust_triple}`",
        searched
            .iter()
            .map(|path| format!("`{}`", path.display()))
            .collect::<Vec<_>>()
            .join(", "),
    ))
}

/// Resolved SDK roots, memoized per SDK name.
#[derive(Default)]
pub(crate) struct SysrootCache {
    entries: Vec<(&'static str, Result<PathBuf, String>)>,
}

impl SysrootCache {
    pub(crate) fn path(&mut self, sdk: &'static str) -> Result<PathBuf, String> {
        if let Some((_, cached)) = self.entries.iter().find(|(name, _)| *name == sdk) {
            return cached.clone();
        }
        let resolved = show_sdk_path(sdk);
        self.entries.push((sdk, resolved.clone()));
        resolved
    }
}

/// Asks Xcode's toolchain where an SDK lives.
///
/// Shared with the live flow, which cross-builds reload artifacts against the
/// same SDKs the export used.
pub(crate) fn sdk_sysroot(sdk: &'static str) -> Result<PathBuf, String> {
    show_sdk_path(sdk)
}

/// Asks Xcode's toolchain where `sdk` lives.
fn show_sdk_path(sdk: &str) -> Result<PathBuf, String> {
    let output = Command::new("xcrun")
        .args(["--sdk", sdk, "--show-sdk-path"])
        .output()
        .map_err(|error| format!("xcrun could not be started: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "`xcrun --sdk {sdk} --show-sdk-path` failed; is Xcode installed?"
        ));
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if text.is_empty() {
        return Err(format!("`xcrun --sdk {sdk}` returned no SDK path"));
    }
    Ok(PathBuf::from(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal 64-bit Mach-O object header: magic, cputype, subtype, file
    /// type, one load command (`LC_BUILD_VERSION`), no flags.
    fn object_with_build_version(platform: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0xfeed_facf_u32.to_le_bytes()); // magic
        bytes.extend_from_slice(&0x0100_000cu32.to_le_bytes()); // cputype arm64
        bytes.extend_from_slice(&0u32.to_le_bytes()); // cpusubtype
        bytes.extend_from_slice(&1u32.to_le_bytes()); // filetype MH_OBJECT
        bytes.extend_from_slice(&1u32.to_le_bytes()); // ncmds
        bytes.extend_from_slice(&24u32.to_le_bytes()); // sizeofcmds
        bytes.extend_from_slice(&0u32.to_le_bytes()); // flags
        bytes.extend_from_slice(&0u32.to_le_bytes()); // reserved
        bytes.extend_from_slice(&LC_BUILD_VERSION.to_le_bytes());
        bytes.extend_from_slice(&24u32.to_le_bytes()); // cmdsize
        bytes.extend_from_slice(&platform.to_le_bytes());
        bytes.extend_from_slice(&0x000b_0000u32.to_le_bytes()); // minos 11.0
        bytes.extend_from_slice(&0u32.to_le_bytes()); // sdk
        bytes.extend_from_slice(&0u32.to_le_bytes()); // ntools
        bytes
    }

    #[test]
    fn restamping_rewrites_only_the_platform_word() {
        let original = object_with_build_version(1); // macOS
        let restamped = restamp_macho_platform(&original, 7).expect("restamps"); // iOS simulator

        assert_ne!(original, restamped, "the platform is the change");
        let platform = u32::from_le_bytes(restamped[40..44].try_into().unwrap());
        assert_eq!(platform, 7);
        // Everything before the platform field — including minos — is intact.
        assert_eq!(&original[..40], &restamped[..40]);
        assert_eq!(original[44..], restamped[44..], "minos and sdk are kept");
    }

    #[test]
    fn a_file_without_a_build_version_passes_through() {
        let plain = vec![0u8; 16];
        assert_eq!(restamp_macho_platform(&plain, 7).as_deref(), Ok(&plain[..]));
    }
}
