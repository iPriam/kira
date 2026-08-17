//! Builds the Kira library this crate embeds, exactly as a real consumer would.
//!
//! A consumer of a Kira library has two ways to reach the generated wrapper
//! crate: depend on `.kira-build/rust/<name>/` by path after running
//! `kira build`, or generate it from a `build.rs` into `OUT_DIR`. This takes the
//! second route, because it is the one that works on a machine that has never
//! run `kira` — which is what makes this crate provable on CI, from a clean
//! checkout, with no LLVM and no prior build step.
//!
//! Both routes produce the identical crate from the identical generator; the
//! only difference is where the files land.
//!
//! # Two engines, one script
//!
//! Which engine is built is the `native-engine` feature, and the branch below is
//! the *only* place the two differ. Off (the default), the library becomes a
//! `.kbc` the wrapper embeds and runs on the VM — no LLVM, no linker. On, it
//! becomes a static archive the wrapper links, and this script emits the cargo
//! directives that make the linker find it. Everything downstream — the same
//! `src/lib.rs` include, the same `tests/consumer.rs` — is untouched by the
//! choice, which is the property the feature exists to have.
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
    /// The library or its wrapper could not be built for the VM engine.
    #[error("the fixture library could not be built: {0}")]
    Library(#[from] kira_build::LibraryBuildError),
    /// The library or its wrapper could not be built for the native engine.
    #[error("the fixture library could not be built as native code: {0}")]
    NativeLibrary(#[from] kira_build::NativeLibraryError),
    /// The library or its wrapper could not be built for the hybrid engine.
    #[error("the fixture library could not be built for the hybrid engine: {0}")]
    HybridLibrary(#[from] kira_build::HybridLibraryError),
    /// Two engine features are on at once.
    #[error(
        "`native-engine` and `hybrid-engine` are both enabled, and they generate the same \
         wrapper: pick one, so a test run says which engine it exercised"
    )]
    TwoEngines,
    /// The Kira native runtime archive is not where a workspace build puts it.
    #[error(
        "cannot find `libkira_native_bridge.a` above `{from}`; build it with \
         `cargo build --workspace`, which is what writes it into the profile \
         directory this build script searches"
    )]
    RuntimeArchiveMissing {
        /// Where the search started.
        from: String,
    },
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

    // Refused rather than resolved by precedence: both features replace the same
    // generated wrapper, so a silent winner would mean a test run reporting an
    // engine it did not exercise — the exact lie the parity suite exists to
    // prevent.
    if cfg!(feature = "native-engine") && cfg!(feature = "hybrid-engine") {
        return Err(BuildError::TwoEngines);
    }
    let wrapper_crate = if cfg!(feature = "native-engine") {
        native(&compiled.ir, name, version, &out)?
    } else if cfg!(feature = "hybrid-engine") {
        hybrid(&compiled.ir, name, version, &out)?
    } else {
        vm(&compiled.ir, name, version, &out)?
    };

    // `src/lib.rs` includes the generated wrapper from here, and the tests read
    // the crate directory to check what was written into it.
    println!(
        "cargo:rustc-env=KIRA_UIFOUNDATION_WRAPPER={}",
        wrapper_crate.join("src").join("lib.rs").display()
    );
    println!(
        "cargo:rustc-env=KIRA_UIFOUNDATION_CRATE={}",
        wrapper_crate.display()
    );
    Ok(())
}

/// Builds the fixture for the VM engine: a `.kbc` the wrapper embeds.
fn vm(
    ir: &kira_ir::IrProgram,
    name: String,
    version: String,
    out: &Path,
) -> Result<PathBuf, BuildError> {
    let artifacts = kira_build::build_library(
        ir,
        &kira_build::LibraryBuildOptions {
            name,
            version,
            // Straight into OUT_DIR rather than into the source tree: a build
            // script that wrote next to its own sources would make `cargo build`
            // dirty the checkout, and two targets building at once would race
            // for the path.
            build_directory: out.to_path_buf(),
            toolchain_root: kira_build::toolchain_root(),
        },
    )?;
    Ok(artifacts.wrapper_crate)
}

/// Builds the fixture for the native engine: a static archive the wrapper links.
///
/// The generated crate's own `build.rs` is not run — this crate `include!`s the
/// wrapper's `src/lib.rs` rather than depending on it — so the cargo directives
/// that generated `build.rs` would have emitted are emitted here instead,
/// against the same archive. That is the one place the `include!` route costs
/// something, and it is worth paying: it is what lets the whole proof run from a
/// clean checkout with no prior `kira`.
fn native(
    ir: &kira_ir::IrProgram,
    name: String,
    version: String,
    out: &Path,
) -> Result<PathBuf, BuildError> {
    let runtime_archive = find_runtime_archive(out)?;
    let artifacts = kira_build::build_native_library(
        ir,
        &kira_build::NativeLibraryOptions {
            name: name.clone(),
            version,
            build_directory: out.to_path_buf(),
            toolchain_root: kira_build::toolchain_root(),
            runtime_archive,
            emit_llvm_ir: false,
            // This crate is a Rust consumer being compiled for the machine
            // running the build, so the library it links must be too.
            target: kira_build::NativeBuildTarget::host(),
        },
    )?;

    let directory = out.join("lib");
    println!("cargo:rustc-link-search=native={}", directory.display());
    println!("cargo:rustc-link-lib=static={name}");
    // The same list the generated `build.rs` renders and the same list the
    // compiler's own linker path uses, read from the crate that owns it. A hand
    // copy here would be a third spelling, and the first library added to one of
    // the other two would fail this crate's link naming nothing.
    let platform = kira_build::host_link_list();
    for library in platform.libraries {
        println!("cargo:rustc-link-lib={library}");
    }
    for framework in platform.frameworks {
        println!("cargo:rustc-link-lib=framework={framework}");
    }
    Ok(artifacts.wrapper_crate)
}

/// Builds the fixture for the hybrid engine: bytecode plus a loaded native half.
///
/// Nothing is emitted for the linker here, and that is the shape of this engine
/// rather than an omission — the native half is `dlopen`ed at run time, not
/// linked at build time. The generated wrapper records where this build put it,
/// as the last entry of a three-place search order, so `cargo test` finds it
/// with no further arrangement. Deployment beyond this directory is the other
/// two entries' job; see `kira_hybrid_main::locate`.
fn hybrid(
    ir: &kira_ir::IrProgram,
    name: String,
    version: String,
    out: &Path,
) -> Result<PathBuf, BuildError> {
    let runtime_archive = find_runtime_archive(out)?;
    let artifacts = kira_build::build_hybrid_library(
        ir,
        &kira_build::HybridLibraryOptions {
            name,
            version,
            build_directory: out.to_path_buf(),
            toolchain_root: kira_build::toolchain_root(),
            runtime_archive,
            emit_llvm_ir: false,
        },
    )?;
    Ok(artifacts.wrapper_crate)
}

/// Finds `libkira_native_bridge.a` in the cargo profile directory above
/// `OUT_DIR`.
///
/// `OUT_DIR` is `target/<profile>/build/<pkg>-<hash>/out`, so the profile
/// directory — where cargo writes a workspace member's `staticlib` — is three
/// levels up. Searched upward rather than computed as three levels, because a
/// build for an explicit target triple adds a directory and a computed depth
/// would find nothing there.
fn find_runtime_archive(out: &Path) -> Result<PathBuf, BuildError> {
    // The name cargo gave it on this host — `<name>.lib` under MSVC. Spelled
    // the Unix way only, this search walks the whole tree and finds nothing on
    // Windows, then reports the archive as missing from a directory that has
    // it. Written out rather than shared with `kira-toolchain`, because a build
    // script is the one place that must not grow a dependency to answer a
    // question about a filename.
    let name = if cfg!(target_env = "msvc") {
        "kira_native_bridge.lib"
    } else {
        "libkira_native_bridge.a"
    };
    let mut directory = Some(out);
    while let Some(current) = directory {
        let candidate = current.join(name);
        if candidate.is_file() {
            println!("cargo:rerun-if-changed={}", candidate.display());
            return Ok(candidate);
        }
        directory = current.parent();
    }
    Err(BuildError::RuntimeArchiveMissing {
        from: out.display().to_string(),
    })
}
