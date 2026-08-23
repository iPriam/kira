//! Links the pinned libffi archive into this crate, statically.
//!
//! Kira links libffi rather than shipping it. A user downloads Kira and has the
//! engine already: there is no `libffi.so.8` beside an artifact to lose, no
//! library search path that could find a different one, and no version on the
//! machine that could shadow the version Kira was built against.
//!
//! What that costs is this script. A static archive has to exist for the machine
//! being compiled *for*, at build time — so a cross build needs an archive it
//! cannot produce from the host's, and the answer is `knvm install libffi`,
//! which fetches the one the libffi fork's CI published for that target.
//!
//! The target is read from `CARGO_CFG_*` rather than from `cfg!`, because this
//! script runs on the host and links for the target, and those are not the same
//! machine on any cross build.

use std::env;
use std::error::Error;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn Error>> {
    // The override first, so a hand-built archive needs no install and no
    // release: this is how the libffi fork itself is tested against Kira before
    // its CI has published anything.
    println!("cargo:rerun-if-env-changed=KIRA_LIBFFI_HOME");

    let os = env::var("CARGO_CFG_TARGET_OS")?;
    let arch = env::var("CARGO_CFG_TARGET_ARCH")?;
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();

    // A target Kira publishes no engine for links none. `raw.rs` is compiled
    // without the engine on such a target and reports the unsupported machine at
    // the point a program actually asks for a foreign call, where the message
    // can name what was wanted.
    let Some(target_key) = kira_toolchain::libffi_vendor_target(&os, &arch) else {
        println!("cargo:rustc-cfg=kira_libffi_unavailable");
        return Ok(());
    };

    let home = libffi_home(target_key)?;
    let archive = kira_toolchain::managed_libffi_archive(&home, &os, &target_env);
    if !archive.is_file() {
        return Err(format!(
            "no libffi archive for `{target_key}` at {}\n\
             Kira links libffi statically, so the archive for the machine being \
             built for has to be installed first:\n    \
             knvm install libffi\n\
             or point KIRA_LIBFFI_HOME at a libffi install tree holding lib/{}",
            archive.display(),
            kira_toolchain::static_archive_name_for(&os, &target_env),
        )
        .into());
    }

    println!("cargo:rerun-if-changed={}", archive.display());
    println!(
        "cargo:rustc-link-search=native={}",
        archive
            .parent()
            .ok_or("the libffi archive has no parent directory")?
            .display()
    );
    println!(
        "cargo:rustc-link-lib=static={}",
        kira_toolchain::link_name_for(&os, &target_env)
    );
    Ok(())
}

/// The libffi install tree this build links out of.
///
/// `KIRA_LIBFFI_HOME` names one outright. Otherwise it is the managed home for
/// the pinned version and this target, which is where `knvm install libffi`
/// puts what it fetches.
fn libffi_home(target_key: &str) -> Result<PathBuf, Box<dyn Error>> {
    if let Some(named) = env::var_os("KIRA_LIBFFI_HOME") {
        return Ok(PathBuf::from(named));
    }
    let version = kira_toolchain::libffi_pinned_version()?;
    Ok(kira_toolchain::managed_libffi_home(version, target_key)?)
}
