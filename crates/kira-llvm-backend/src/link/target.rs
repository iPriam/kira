//! Where a native build is aimed: the machine whose binaries it produces, and
//! the sysroot the system headers and libraries for that machine come from.
//!
//! Every decision the link and the C-shim compile make used to read `cfg!` —
//! `cfg!(target_os = "macos")` for the shared-library flag, `cfg!(target_env =
//! "msvc")` for the symbol-export spelling, `std::env::consts::OS` for the
//! platform library list. Those all answer about the machine running the
//! compiler, which is the right answer exactly when the two machines are the
//! same one. This value is what they read instead, so a build for another
//! machine gets that machine's answers and a host build gets the ones it always
//! got.

use std::path::PathBuf;

use kira_backend_api::{Linkage, NativeTarget, RelocationModel};

/// The environment variable that names the sysroot a cross link uses when the
/// invocation did not name one.
///
/// A sysroot is a property of the machine doing the building, not of the program
/// being built: the same package cross-compiled on a Fedora host, a Debian host,
/// and a container finds its aarch64 C library in three different places, and
/// none of them belongs in a `package.kira`. So it is an environment setting
/// with a command-line override, the way `SDKROOT` already is on macOS, and
/// there is no default path compiled in — a wrong guess produces a link against
/// the *host's* C library, which fails with a page of unreadable relocation
/// errors rather than with "there is no sysroot here".
pub const SYSROOT_VARIABLE: &str = "KIRA_SYSROOT";

/// The machine a native build emits and links for, and where its system
/// libraries live.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NativeBuildTarget {
    target: NativeTarget,
    sysroot: Option<PathBuf>,
}

impl NativeBuildTarget {
    /// The machine running the compiler, with no sysroot override.
    ///
    /// What every artifact this process itself loads is built for: the shared
    /// carrier a live VM opens, the hybrid half the interpreter calls into, the
    /// whole-program library the desktop runner maps. Those are not a target
    /// selection at all — they run here or they run nowhere — so they name the
    /// host rather than threading a choice they cannot honour.
    #[must_use]
    pub fn host() -> Self {
        Self::default()
    }

    /// A build for `target`, with `sysroot` overriding [`SYSROOT_VARIABLE`].
    #[must_use]
    pub fn new(target: NativeTarget, sysroot: Option<PathBuf>) -> Self {
        Self { target, sysroot }
    }

    /// Which machine this build is for.
    #[must_use]
    pub fn target(&self) -> &NativeTarget {
        &self.target
    }

    /// Whether this build emits for the machine running the compiler.
    #[must_use]
    pub fn is_host(&self) -> bool {
        self.target.is_host()
    }

    /// The `target_os` of the machine being built for, spelled as
    /// `cfg(target_os = ...)` and [`crate::platform::link_list_for`] spell it.
    #[must_use]
    pub fn target_os(&self) -> &str {
        match self.target.cross() {
            None => std::env::consts::OS,
            Some(cross) => cross.triple().os(),
        }
    }

    /// Whether the linked image is a Mach-O, whose linker and symbol spellings
    /// are Apple's own.
    #[must_use]
    pub fn is_macos(&self) -> bool {
        self.target_os() == "macos"
    }

    /// Whether the linked image is a PE built by the MSVC toolchain, which
    /// exports nothing it was not explicitly told to and takes `link.exe`'s
    /// flags rather than a Unix linker's.
    #[must_use]
    pub fn is_msvc(&self) -> bool {
        match self.target.cross() {
            None => cfg!(target_env = "msvc"),
            Some(cross) => cross.triple().abi() == "msvc",
        }
    }

    /// Whether the linked image is a PE, whatever produced it.
    #[must_use]
    pub fn is_windows(&self) -> bool {
        self.target_os() == "windows"
    }

    /// Whether this build folds its libraries into the image rather than
    /// resolving them at startup.
    ///
    /// A host build is never one: this machine has a loader, and the artifacts
    /// the compiler itself loads are shared objects by construction.
    #[must_use]
    pub fn is_statically_linked(&self) -> bool {
        self.target
            .cross()
            .is_some_and(|cross| cross.linkage() == Linkage::Static)
    }

    /// The sysroot this build's system headers and libraries come from, or
    /// `None` when the driver's own defaults are the right answer.
    ///
    /// A host build always answers `None`: the managed clang finds this
    /// machine's libraries by itself everywhere except macOS, which has its own
    /// SDK lookup, and honouring a cross-compilation environment variable on a
    /// host build would silently redirect an ordinary `kira build` at somebody
    /// else's C library.
    #[must_use]
    pub fn sysroot(&self) -> Option<PathBuf> {
        if self.is_host() {
            return None;
        }
        if let Some(explicit) = &self.sysroot {
            return Some(explicit.clone());
        }
        let named = std::env::var_os(SYSROOT_VARIABLE)?;
        if named.is_empty() {
            return None;
        }
        Some(PathBuf::from(named))
    }

    /// Which setting chose the sysroot, named the way a person would correct it.
    ///
    /// A diagnostic about a sysroot that is not there has to say where the path
    /// came from: the path itself is the one part the reader already knows is
    /// wrong, and there are two places it could have been set.
    #[must_use]
    pub fn sysroot_setting(&self) -> &'static str {
        if self.sysroot.is_some() {
            "the `--sysroot` argument"
        } else {
            "the `KIRA_SYSROOT` environment variable"
        }
    }

    /// The driver arguments that aim a *compile* at this machine.
    ///
    /// Shared with the link line rather than duplicated, because the generated
    /// C shim is compiled with one and linked with the other, and a shim
    /// compiled for the host and linked into a cross binary is an object the
    /// linker rejects for the one reason a diagnostic never states plainly: it
    /// is the right code for the wrong machine.
    #[must_use]
    pub fn compile_arguments(&self) -> Vec<String> {
        let Some(cross) = self.target.cross() else {
            return Vec::new();
        };
        let mut arguments = vec![format!("--target={}", cross.normalized_triple())];
        if let Some(sysroot) = self.sysroot() {
            arguments.push(format!("--sysroot={}", sysroot.display()));
        }
        // Absolute addressing has to be asked for in the C shim too. The shim
        // is compiled by the same driver that links the program, and a
        // position-independent object in an image with no dynamic loader is the
        // half of the program that faults.
        if cross.relocation() == RelocationModel::Static {
            arguments.push("-fno-pic".to_owned());
        }
        arguments
    }

    /// The driver arguments that aim a *link* at this machine.
    ///
    /// A cross link is also told which linker to use, which the host link is
    /// not. clang picks a linker, it does not contain one, and left alone it
    /// searches `PATH` for `ld` — so a cross link from a Windows host handed its
    /// freshly emitted ELF object to whatever PE linker happened to be
    /// installed, which answered `unrecognised emulation mode: elf_x86_64` and
    /// said nothing about targets, sysroots, or Kira. `lld` ships in the managed
    /// bundle and links every format Kira emits, from every host it runs on.
    #[must_use]
    pub fn link_arguments(&self) -> Vec<String> {
        let mut arguments = self.compile_arguments();
        if !self.is_host() {
            arguments.push("-fuse-ld=lld".to_owned());
        }
        if let Some(cross) = self.target.cross()
            && cross.relocation() == RelocationModel::Static
        {
            // The relocation model decides how code addresses things; this
            // decides what the linker makes of the result. A PIE built from
            // absolutely-addressed objects links and then starts at whatever
            // address the loader picked, which is not the one the code assumes.
            arguments.push("-no-pie".to_owned());
        }
        if let Some(cross) = self.target.cross()
            && cross.linkage() == Linkage::Static
        {
            // Folds every library into the image, so the program names no
            // interpreter and needs no shared object present to start. A
            // freestanding userland has neither: `-no-pie` alone still produces
            // a binary whose `PT_INTERP` names a loader that is not there, and
            // the kernel refuses it before `main` — with no output, because the
            // program never ran to produce any.
            arguments.push("-static".to_owned());
        }
        arguments
    }
}

/// The sysroot a *host* macOS link uses, or `None` anywhere else.
///
/// Asks `xcrun`, the same way every other macOS toolchain finds the SDK, and
/// honours an explicit `SDKROOT` first. Returning `None` when `xcrun` cannot
/// answer leaves the driver to its own defaults rather than passing a path that
/// does not exist.
///
/// Only the host path consults it. A build aimed at macOS from somewhere else
/// has no `xcrun` to ask and no Xcode to ask about, so its SDK is named the way
/// every other cross target's is — through [`NativeBuildTarget::sysroot`].
pub(super) fn macos_sysroot(target: &NativeBuildTarget) -> Option<PathBuf> {
    if !target.is_host() || !cfg!(target_os = "macos") {
        return None;
    }
    if let Some(root) = std::env::var_os("SDKROOT")
        && !root.is_empty()
    {
        return Some(PathBuf::from(root));
    }
    let output = std::process::Command::new("xcrun")
        .arg("--show-sdk-path")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8(output.stdout).ok()?;
    let trimmed = path.trim();
    (!trimmed.is_empty()).then(|| PathBuf::from(trimmed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use kira_backend_api::CrossTarget;
    use kira_native_lib_definition::TargetTriple;

    fn cross(text: &str, relocation: RelocationModel) -> NativeBuildTarget {
        linked(text, relocation, Linkage::Dynamic)
    }

    fn linked(text: &str, relocation: RelocationModel, linkage: Linkage) -> NativeBuildTarget {
        NativeBuildTarget::new(
            NativeTarget::Cross(CrossTarget::new(
                TargetTriple::parse(text).expect("a valid triple"),
                relocation,
                linkage,
            )),
            None,
        )
    }

    /// A host build's driver line is exactly what it was before cross targets
    /// existed: no `--target`, no `--sysroot`, no relocation flags.
    #[test]
    fn a_host_build_adds_no_driver_arguments_at_all() {
        let host = NativeBuildTarget::host();
        assert!(host.is_host());
        assert!(host.compile_arguments().is_empty());
        assert!(host.link_arguments().is_empty());
        assert_eq!(host.sysroot(), None);
        assert_eq!(host.target_os(), std::env::consts::OS);
    }

    #[test]
    fn a_cross_build_aims_the_driver_at_the_named_machine() {
        let target = cross("aarch64-linux-gnu", RelocationModel::Pic);
        assert_eq!(
            target.compile_arguments(),
            ["--target=aarch64-unknown-linux-gnu"]
        );
        assert_eq!(
            target.link_arguments(),
            ["--target=aarch64-unknown-linux-gnu", "-fuse-ld=lld"]
        );
        assert_eq!(target.target_os(), "linux");
        assert!(!target.is_macos() && !target.is_msvc() && !target.is_windows());
    }

    /// Absolute addressing is asked for on both lines: `-fno-pic` so the shim's
    /// code does not go through a global offset table, and `-no-pie` so the
    /// image the linker produces is loaded where the code expects to be.
    #[test]
    fn a_static_target_asks_for_absolute_addressing_on_both_lines() {
        let target = cross("aarch64-linux-gnu", RelocationModel::Static);
        assert_eq!(
            target.compile_arguments(),
            ["--target=aarch64-unknown-linux-gnu", "-fno-pic"]
        );
        assert_eq!(
            target.link_arguments(),
            [
                "--target=aarch64-unknown-linux-gnu",
                "-fno-pic",
                "-fuse-ld=lld",
                "-no-pie"
            ]
        );
    }

    /// The two freestanding settings are separate answers and appear separately:
    /// linkage never implies absolute addressing, and a statically linked PIE is
    /// a real and ordinary thing to want.
    #[test]
    fn a_static_linkage_folds_the_libraries_in_without_touching_addressing() {
        let target = linked("aarch64-linux-gnu", RelocationModel::Pic, Linkage::Static);
        assert_eq!(
            target.compile_arguments(),
            ["--target=aarch64-unknown-linux-gnu"]
        );
        assert_eq!(
            target.link_arguments(),
            [
                "--target=aarch64-unknown-linux-gnu",
                "-fuse-ld=lld",
                "-static"
            ]
        );
    }

    /// What a userland with no loader asks for: absolute addressing so the image
    /// runs where it was linked, and a static link so there is nothing left to
    /// resolve at startup.
    #[test]
    fn a_freestanding_target_asks_for_both() {
        let target = linked(
            "aarch64-linux-gnu",
            RelocationModel::Static,
            Linkage::Static,
        );
        assert_eq!(
            target.link_arguments(),
            [
                "--target=aarch64-unknown-linux-gnu",
                "-fno-pic",
                "-fuse-ld=lld",
                "-no-pie",
                "-static"
            ]
        );
    }

    #[test]
    fn an_explicit_sysroot_reaches_both_the_compile_and_the_link() {
        let target = NativeBuildTarget::new(
            NativeTarget::Cross(CrossTarget::new(
                TargetTriple::parse("aarch64-linux-gnu").expect("a valid triple"),
                RelocationModel::Pic,
                Linkage::Dynamic,
            )),
            Some(PathBuf::from("/usr/aarch64-linux-gnu")),
        );
        assert_eq!(
            target.sysroot(),
            Some(PathBuf::from("/usr/aarch64-linux-gnu"))
        );
        assert!(
            target
                .compile_arguments()
                .iter()
                .any(|argument| argument == "--sysroot=/usr/aarch64-linux-gnu"),
            "{:?}",
            target.compile_arguments()
        );
    }

    /// The object format follows the target, not this machine. A Windows cross
    /// target has to be recognised as one from a Linux host, or the link goes
    /// out without the `/EXPORT:` flags a DLL needs and produces a library that
    /// resolves nothing by name.
    #[test]
    fn the_object_formats_questions_are_answered_about_the_target() {
        let windows = cross("x86_64-windows-msvc", RelocationModel::Pic);
        assert!(windows.is_windows() && windows.is_msvc() && !windows.is_macos());

        let apple = cross("aarch64-macos-none", RelocationModel::Pic);
        assert!(apple.is_macos() && !apple.is_msvc() && !apple.is_windows());
    }

    /// `xcrun` is a host-only lookup: a build aimed at macOS from another
    /// machine has no Xcode to ask and names its SDK explicitly instead.
    #[test]
    fn the_xcode_sdk_lookup_never_answers_for_a_cross_target() {
        assert_eq!(
            macos_sysroot(&cross("aarch64-macos-none", RelocationModel::Pic)),
            None
        );
    }
}
