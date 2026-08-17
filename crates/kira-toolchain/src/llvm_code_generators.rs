//! Which code generators a managed LLVM bundle was built with.
//!
//! `llvm-metadata.toml` pins the list as `build.targets_to_build`, and the
//! release workflow builds every published bundle from it. The two can still
//! disagree on a machine: a release owns its assets for good, so a pin that
//! grows a code generator after a release was cut leaves installs whose LLVM
//! is a perfectly good LLVM carrying fewer generators than the pin now names.
//! That is exactly the state every bundle published before the pin named X86
//! and AArch64 outright is in, and it is why a cross build reports a missing
//! code generator by name instead of failing somewhere inside LLVM.
//!
//! Nothing may assume the answer. The backend's build script links the
//! initializers the bundle actually defines — the alternative is a linker
//! failure naming four LLVM symbols — and the tools that provision a bundle
//! read the same answer, so the missing generator is reported where it can be
//! acted on rather than discovered by a build that cannot finish.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::executable_name;
use crate::llvm_metadata::{MalformedMetadata, pinned};

/// LLVM's name for the code generator that emits WebAssembly — Kira's Web
/// device, emitted in process by the native backend.
pub const WEB_CODE_GENERATOR: &str = "WebAssembly";

/// LLVM's name for the code generator of a host architecture, as
/// `std::env::consts::ARCH` spells it.
///
/// `None` for an architecture Kira publishes no bundle for; such a host has no
/// managed LLVM to ask about in the first place.
#[must_use]
pub fn code_generator_for_arch(arch: &str) -> Option<&'static str> {
    match arch {
        "x86_64" | "x86" => Some("X86"),
        "aarch64" => Some("AArch64"),
        _ => None,
    }
}

/// LLVM's name for this host's code generator.
#[must_use]
pub fn host_code_generator() -> Option<&'static str> {
    code_generator_for_arch(std::env::consts::ARCH)
}

/// The code generators the pin requires a bundle to carry, on this host.
///
/// `host` in `targets_to_build` is CMake's spelling for "the machine doing the
/// build", so it expands to this host's generator; every other entry is an
/// LLVM target name used as written.
pub fn pinned_code_generators() -> Result<Vec<&'static str>, MalformedMetadata> {
    Ok(expand(
        &pinned()?.build.targets_to_build,
        host_code_generator(),
    ))
}

/// Splits a `targets_to_build` list, expanding `host` to `host_generator`.
fn expand(
    targets_to_build: &'static str,
    host_generator: Option<&'static str>,
) -> Vec<&'static str> {
    targets_to_build
        .split(';')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .filter_map(|name| {
            if name.eq_ignore_ascii_case("host") {
                host_generator
            } else {
                Some(name)
            }
        })
        .collect()
}

/// Why a bundle's code generators could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CodeGeneratorError {
    /// The bundle ships no `llvm-config`, so it can only be asked by guessing.
    #[error("the LLVM at `{}` ships no `llvm-config`, so what it was built with cannot be read", .home.display())]
    NoLlvmConfig {
        /// The install root that was looked in.
        home: PathBuf,
    },
    /// `llvm-config` could not be run at all.
    #[error("cannot run `{}`: {detail}", .llvm_config.display())]
    Unrunnable {
        /// The tool that was invoked.
        llvm_config: PathBuf,
        /// What the spawn reported.
        detail: String,
    },
    /// `llvm-config` ran and refused.
    #[error("`{} --targets-built` failed: {detail}", .llvm_config.display())]
    Refused {
        /// The tool that was invoked.
        llvm_config: PathBuf,
        /// What it printed on stderr.
        detail: String,
    },
    /// The compiled-in pin could not be read.
    #[error(transparent)]
    Metadata(#[from] MalformedMetadata),
}

/// The code generators a bundle was built with, as its own `llvm-config`
/// reports them.
pub fn built(llvm_config: &Path) -> Result<Vec<String>, CodeGeneratorError> {
    let output = Command::new(llvm_config)
        .arg("--targets-built")
        .output()
        .map_err(|error| CodeGeneratorError::Unrunnable {
            llvm_config: llvm_config.to_path_buf(),
            detail: error.to_string(),
        })?;
    if !output.status.success() {
        return Err(CodeGeneratorError::Refused {
            llvm_config: llvm_config.to_path_buf(),
            detail: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    Ok(parse_targets_built(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

/// The names on an `llvm-config --targets-built` line.
fn parse_targets_built(line: &str) -> Vec<String> {
    line.split_whitespace().map(str::to_owned).collect()
}

/// The pinned code generators a bundle at `home` does not carry.
///
/// An empty result means the bundle matches the pin. The `llvm-config` is the
/// bundle's own, because the question is about that tree and not about
/// whatever LLVM the host has on `PATH`.
pub fn missing_from(home: &Path) -> Result<Vec<&'static str>, CodeGeneratorError> {
    let llvm_config = home.join("bin").join(executable_name("llvm-config"));
    if !llvm_config.is_file() {
        return Err(CodeGeneratorError::NoLlvmConfig {
            home: home.to_path_buf(),
        });
    }
    let built = built(&llvm_config)?;
    Ok(missing_among(&built, &pinned_code_generators()?))
}

/// The `required` generators that `built` does not carry.
///
/// LLVM prints the names it registers them under, which is the same spelling
/// `LLVM_TARGETS_TO_BUILD` takes, so the comparison is exact rather than
/// case-folded — `WebAssembly` is one name, not a word to normalize.
fn missing_among(built: &[String], required: &[&'static str]) -> Vec<&'static str> {
    required
        .iter()
        .filter(|name| !built.iter().any(|had| had == *name))
        .copied()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_names_off_a_targets_built_line() {
        assert_eq!(
            parse_targets_built("X86 WebAssembly\n"),
            vec!["X86".to_owned(), "WebAssembly".to_owned()]
        );
        assert!(parse_targets_built("\n").is_empty());
    }

    #[test]
    fn expands_host_to_this_hosts_generator() {
        assert_eq!(
            expand("host;WebAssembly", Some("X86")),
            ["X86", "WebAssembly"]
        );
        assert_eq!(expand("host", Some("AArch64")), ["AArch64"]);
        // A host Kira publishes no bundle for contributes no name rather than
        // a literal `host`, which is not an LLVM target and would report every
        // bundle as incomplete.
        assert_eq!(expand("host;WebAssembly", None), ["WebAssembly"]);
    }

    #[test]
    fn a_bundle_built_before_the_pin_grew_a_generator_reports_it() {
        let built = ["X86".to_owned()];
        assert_eq!(
            missing_among(&built, &["X86", "WebAssembly"]),
            ["WebAssembly"]
        );
        assert!(missing_among(&built, &["X86"]).is_empty());
    }

    /// The pin names the Web device's code generator, and the backend emits
    /// wasm objects in process with it. A pin that dropped it would leave the
    /// `--device wasm32` path with nothing to be built from.
    #[test]
    fn the_pin_requires_the_web_code_generator() {
        let required = pinned_code_generators().expect("the pin parses");
        assert!(
            required.contains(&WEB_CODE_GENERATOR),
            "the pin must name {WEB_CODE_GENERATOR}, got {required:?}"
        );
    }

    #[test]
    fn every_published_bundles_host_has_a_code_generator_name() {
        for arch in ["x86_64", "aarch64"] {
            assert!(code_generator_for_arch(arch).is_some());
        }
        assert!(code_generator_for_arch("mips64").is_none());
    }

    /// The pin names every published host's code generator, not just the one
    /// belonging to whichever runner built a given bundle.
    ///
    /// This is what makes `kira build --target <triple>` a property of the
    /// compiler rather than of the download it links: with a bare `host` entry,
    /// the x86_64 Linux bundle carried X86 alone and could never emit an
    /// aarch64 binary, so the same command succeeded or failed depending on
    /// which archive the machine happened to install.
    #[test]
    fn the_pin_requires_every_published_hosts_code_generator() {
        let required = pinned_code_generators().expect("the pin parses");
        for arch in ["x86_64", "aarch64"] {
            let generator =
                code_generator_for_arch(arch).expect("a published host has a code generator");
            assert!(
                required.contains(&generator),
                "the pin must name {generator}, the code generator for {arch} hosts, \
                 got {required:?}",
            );
        }
    }
}
