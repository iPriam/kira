//! Which machine a native build emits code for and links binaries for.
//!
//! Kira spells a target as `arch-os-abi` everywhere a person writes one — a
//! `package.kira` manifest, a `NativeLibs/*.toml` row, `kira build --target`.
//! That is [`kira_native_lib_definition::TargetTriple`], and it is deliberately
//! shorter than the triple a toolchain takes, because the vendor field carries
//! no decision for a package author to make: `aarch64-linux-gnu` names one
//! machine, and whether the tools spell it `aarch64-unknown-linux-gnu` is the
//! tools' business. [`CrossTarget::normalized_triple`] is where the two meet.
//!
//! The types here live at layer 3 rather than in the LLVM backend because the
//! selection is made in the CLI, at layer 9, and travels down through this
//! seam. Nothing in this module knows what a code generator is: an LLVM target
//! name is the backend's vocabulary, and the backend maps the architecture to
//! one when it registers the generators it links.

use std::fmt;

use kira_native_lib_definition::TargetTriple;
use kira_runtime_abi::ForeignPointerWidth;

/// How the code generator forms the addresses in an emitted object.
///
/// This is a real choice for a cross target and not one for the host. Every
/// machine Kira runs on links position-independent executables — required on
/// macOS, the default on modern Linux distributions — so the host path fixes
/// PIC and there is nothing to select. A userland that boots on a machine of
/// its own is the case that differs: it may have no dynamic loader to apply
/// relocations, and a program built PIC for it is one that starts and then
/// jumps through addresses nobody filled in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum RelocationModel {
    /// Position-independent code, linked as a PIE. The default, and what every
    /// ordinary Linux, macOS, and Windows program is.
    #[default]
    Pic,
    /// Absolute addresses, linked as a non-PIE. What a freestanding userland
    /// with no dynamic loader needs.
    Static,
}

impl RelocationModel {
    /// This model's spelling on the command line.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Pic => "pic",
            Self::Static => "static",
        }
    }

    /// Resolves a `--relocation-model` value, or `None` for an unknown one.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "pic" => Some(Self::Pic),
            "static" => Some(Self::Static),
            _ => None,
        }
    }
}

impl fmt::Display for RelocationModel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

/// A machine that is not the one running the compiler.
///
/// Carries the relocation model with the triple rather than beside it, because
/// the two are one decision: `aarch64-linux-gnu` built PIC and the same triple
/// built for a loaderless userland are different artifacts, and every step that
/// takes one — the target machine, the linker driver, the C shim compiler — has
/// to agree on which was asked for.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CrossTarget {
    triple: TargetTriple,
    relocation: RelocationModel,
}

impl CrossTarget {
    /// Names a cross target by its `arch-os-abi` triple and relocation model.
    #[must_use]
    pub fn new(triple: TargetTriple, relocation: RelocationModel) -> Self {
        Self { triple, relocation }
    }

    /// The `arch-os-abi` triple, which is also what native-library rows are
    /// selected by.
    #[must_use]
    pub fn triple(&self) -> &TargetTriple {
        &self.triple
    }

    /// How this target's code forms addresses.
    #[must_use]
    pub fn relocation(&self) -> RelocationModel {
        self.relocation
    }

    /// How wide a pointer is on this machine.
    ///
    /// Every C-layout aggregate offset the lowering computes is baked in at this
    /// width, so it has to be the *target*'s and not the compiler's: a struct
    /// with a pointer member laid out at eight bytes and read at four is a field
    /// read that lands in the middle of the previous one. The Web path already
    /// carries its own width for exactly this reason.
    ///
    /// The 32-bit architectures are named and everything else is 64-bit, which
    /// is the way round that fails safely: an architecture Kira has no code
    /// generator for is refused by the backend before it reaches here, so the
    /// default only ever applies to a 64-bit machine — whereas defaulting to 32
    /// would silently mislay every field past the first pointer.
    #[must_use]
    pub fn pointer_width(&self) -> ForeignPointerWidth {
        match self.triple.arch() {
            "x86" | "arm" | "wasm32" | "riscv32" => ForeignPointerWidth::Bits32,
            _ => ForeignPointerWidth::Bits64,
        }
    }

    /// The `arch-vendor-os-abi` spelling LLVM, clang, and rustc all take.
    ///
    /// The vendor is not information Kira asks anybody for: it follows from the
    /// operating system, and every toolchain agrees on which one goes with
    /// which. Apple's is `apple` and its system is `darwin` rather than
    /// `macos`; Windows' is `pc`; everything else is `unknown`. Deriving it here
    /// rather than making a package author write it is what keeps one triple
    /// spelling in manifests, on the command line, and in diagnostics.
    ///
    /// It is one function and not two because the answer is the same for the
    /// linker driver's `--target=` and for the `cargo build --target` that
    /// produces this target's runtime archive. A second mapping would be a
    /// second place for `aarch64-unknown-linux-gnu` to be spelled slightly
    /// differently, and the failure that produces is an archive built for a
    /// target the link never looks for.
    #[must_use]
    pub fn normalized_triple(&self) -> String {
        let arch = self.triple.arch();
        let abi = self.triple.abi();
        match self.triple.os() {
            "macos" => format!("{arch}-apple-darwin"),
            "ios" => format!("{arch}-apple-ios"),
            "windows" => format!("{arch}-pc-windows-{abi}"),
            os => format!("{arch}-unknown-{os}-{abi}"),
        }
    }
}

impl fmt::Display for CrossTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.triple)
    }
}

/// Which machine a native build emits code for.
///
/// `Host` is not the same value as a [`CrossTarget`] naming this machine, and
/// collapsing the two would be a regression rather than a simplification. A
/// host build asks LLVM for the default triple, the host CPU, and the host CPU's
/// feature string, so it emits code tuned for the processor it is running on; a
/// cross build has no such processor to ask about and emits for the
/// architecture's generic CPU. Naming the host explicitly is what keeps that
/// difference from depending on whether a triple happened to be typed out.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub enum NativeTarget {
    /// The machine running the compiler.
    #[default]
    Host,
    /// Another machine, named on the command line or by a package manifest.
    Cross(CrossTarget),
}

impl NativeTarget {
    /// The cross target, or `None` when this build is for the host.
    #[must_use]
    pub fn cross(&self) -> Option<&CrossTarget> {
        match self {
            Self::Host => None,
            Self::Cross(target) => Some(target),
        }
    }

    /// Whether this build emits for the machine running the compiler.
    #[must_use]
    pub fn is_host(&self) -> bool {
        matches!(self, Self::Host)
    }
}

impl fmt::Display for NativeTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Host => formatter.write_str("host"),
            Self::Cross(target) => write!(formatter, "{target}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cross(text: &str) -> CrossTarget {
        CrossTarget::new(
            TargetTriple::parse(text).expect("a valid triple"),
            RelocationModel::Pic,
        )
    }

    /// The one mapping every tool downstream depends on: the linker driver's
    /// `--target=`, LLVM's own triple lookup, and the `cargo build --target`
    /// that produces the runtime archive all read this spelling.
    #[test]
    fn a_manifest_triple_normalizes_to_the_spelling_toolchains_take() {
        assert_eq!(
            cross("aarch64-linux-gnu").normalized_triple(),
            "aarch64-unknown-linux-gnu"
        );
        assert_eq!(
            cross("x86_64-linux-musl").normalized_triple(),
            "x86_64-unknown-linux-musl"
        );
        assert_eq!(
            cross("aarch64-macos-none").normalized_triple(),
            "aarch64-apple-darwin"
        );
        assert_eq!(
            cross("x86_64-windows-msvc").normalized_triple(),
            "x86_64-pc-windows-msvc"
        );
    }

    #[test]
    fn the_pointer_width_is_the_targets_and_not_this_machines() {
        assert_eq!(
            cross("aarch64-linux-gnu").pointer_width(),
            ForeignPointerWidth::Bits64
        );
        assert_eq!(
            cross("x86-linux-gnu").pointer_width(),
            ForeignPointerWidth::Bits32
        );
    }

    #[test]
    fn a_relocation_model_round_trips_through_its_command_line_spelling() {
        for model in [RelocationModel::Pic, RelocationModel::Static] {
            assert_eq!(RelocationModel::parse(model.label()), Some(model));
        }
        assert_eq!(RelocationModel::parse("pie"), None);
        assert_eq!(RelocationModel::default(), RelocationModel::Pic);
    }

    /// The relocation model is part of the target's identity, so two builds of
    /// one triple that form addresses differently are two targets.
    #[test]
    fn the_relocation_model_distinguishes_two_targets_with_one_triple() {
        let triple = TargetTriple::parse("aarch64-linux-gnu").expect("a valid triple");
        let position_independent = CrossTarget::new(triple.clone(), RelocationModel::Pic);
        let absolute = CrossTarget::new(triple, RelocationModel::Static);
        assert_ne!(position_independent, absolute);
        assert_eq!(
            position_independent.normalized_triple(),
            absolute.normalized_triple()
        );
    }

    #[test]
    fn the_host_is_its_own_selection_and_names_itself() {
        assert!(NativeTarget::default().is_host());
        assert_eq!(NativeTarget::default().cross(), None);
        assert_eq!(NativeTarget::Host.to_string(), "host");

        let target = NativeTarget::Cross(cross("aarch64-linux-gnu"));
        assert!(!target.is_host());
        assert_eq!(target.to_string(), "aarch64-linux-gnu");
    }
}
