//! Builds the Kira library this crate embeds, exactly as a real consumer would.
//!
//! A consumer of a Kira library has two ways to reach the generated wrapper
//! crate: depend on `.kira-build/rust/<name>/` by path after running
//! `kirac build`, or generate it from a `build.rs` into `OUT_DIR`. This takes the
//! second route, because it is the one that works on a machine that has never
//! run `kirac` — which is what makes this crate provable on CI, from a clean
//! checkout, with no LLVM and no prior build step.
//!
//! Both routes produce the identical crate from the identical generator; the
//! only difference is where the files land.
//!
//! # Why it reports rather than panics
//!
//! A failing build script has one job: say what went wrong, once, in a line a
//! person can act on. A panic here prints a backtrace-shaped message about a
//! file nobody was reading, so every failure below is a typed error rendered to
//! stderr, and the script exits non-zero — which is what cargo reads anyway.

use std::path::{Path, PathBuf};

fn main() {
    if let Err(error) = generate() {
        eprintln!("kira-export-consumer: {error}");
        std::process::exit(1);
    }
}

/// Why the embedded library could not be prepared.
#[derive(Debug, thiserror::Error)]
enum BuildError {
    /// Cargo did not set `OUT_DIR`, so there is nowhere to generate into.
    #[error("cargo set no OUT_DIR")]
    NoOutDir,
    /// The fixture library did not reach the frontend.
    #[error("the fixture library could not be compiled: {0}")]
    Frontend(#[from] kira_build::FrontendError),
    /// The fixture library has compile errors.
    #[error("the fixture library has {count} diagnostics, the first being: {first}")]
    Diagnostics {
        /// How many the frontend reported.
        count: usize,
        /// The first one, rendered flat.
        first: String,
    },
    /// The fixture is not inside a package that names it.
    #[error("the fixture has no `package.kira` naming it, so nothing names the generated crate")]
    Unnamed,
    /// The library or its wrapper could not be built.
    #[error("the fixture library could not be built: {0}")]
    Library(#[from] kira_build::LibraryBuildError),
}

/// Compiles the fixture library and generates the wrapper crate into `OUT_DIR`.
fn generate() -> Result<(), BuildError> {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixture")
        .join("uifoundation");
    let source = fixture.join("uifoundation.kira");
    println!("cargo:rerun-if-changed={}", source.display());
    println!(
        "cargo:rerun-if-changed={}",
        fixture.join("package.kira").display()
    );

    let out = PathBuf::from(std::env::var_os("OUT_DIR").ok_or(BuildError::NoOutDir)?);

    let compiled = kira_build::compile(&source)?;
    if compiled.has_errors() {
        return Err(BuildError::Diagnostics {
            count: compiled.diagnostics.len(),
            first: compiled
                .diagnostics
                .first()
                .map(|diagnostic| diagnostic.message.clone())
                .unwrap_or_default(),
        });
    }
    let (Some(name), Some(version)) = (
        compiled.package_name.clone(),
        compiled.package_version.clone(),
    ) else {
        return Err(BuildError::Unnamed);
    };

    let artifacts = kira_build::build_library(
        &compiled.ir,
        &kira_build::LibraryBuildOptions {
            name,
            version,
            // Straight into OUT_DIR rather than into the source tree: a build
            // script that wrote next to its own sources would make `cargo build`
            // dirty the checkout, and two targets building at once would race
            // for the path.
            build_directory: out,
            toolchain_root: kira_build::toolchain_root(),
        },
    )?;

    // `src/lib.rs` includes the generated wrapper from here, and the tests read
    // the crate directory to check what was written into it.
    println!(
        "cargo:rustc-env=KIRA_UIFOUNDATION_WRAPPER={}",
        artifacts.wrapper_crate.join("src").join("lib.rs").display()
    );
    println!(
        "cargo:rustc-env=KIRA_UIFOUNDATION_CRATE={}",
        artifacts.wrapper_crate.display()
    );
    Ok(())
}
