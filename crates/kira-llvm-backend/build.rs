//! Links the managed LLVM into the backend, replacing `llvm-sys`'s own
//! environment-driven search.
//!
//! `llvm-sys` is compiled with `no-llvm-linking` + `disable-alltargets-init`,
//! which turns it into pure declarations that consult no environment at all.
//! This script supplies what it no longer does: it locates the bundle through
//! `kira-toolchain`'s discovery — `KIRA_LLVM_HOME` override first, then the
//! managed install under `~/.kira/toolchains/llvm/<pinned>/<host>` — and asks
//! that bundle's own `llvm-config` for the link line. So a plain `cargo
//! build` in any shell, editor, or CI job finds LLVM through Kira's code,
//! with nothing exported and nothing on PATH.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    // The override is the one environment input discovery honors; the pin
    // file is what moves when the LLVM version does.
    println!("cargo:rerun-if-env-changed=KIRA_LLVM_HOME");
    let repo_root = repo_root();
    println!(
        "cargo:rerun-if-changed={}",
        repo_root.join("llvm-metadata.toml").display()
    );

    let installation = match kira_toolchain::llvm_discovery::discover(Some(&repo_root)) {
        Ok(installation) => installation,
        Err(error) => fail(&error.to_string()),
    };
    let Some(llvm_config) = installation.llvm_config else {
        fail(&format!(
            "the managed LLVM at `{}` ships no `llvm-config`",
            installation.home.display()
        ));
    };

    println!(
        "cargo:rustc-link-search=native={}",
        run(&llvm_config, &["--libdir"]).trim()
    );
    // The bundle's LLVM archives are linked statically: a `kirac` must run
    // without the bundle installed beside it. The system libraries LLVM
    // itself needs stay dynamic — they are the host's, not the bundle's.
    for name in link_names(&run(&llvm_config, &["--link-static", "--libs"])) {
        println!("cargo:rustc-link-lib=static={name}");
    }
    for name in link_names(&run(&llvm_config, &["--link-static", "--system-libs"])) {
        println!("cargo:rustc-link-lib=dylib={name}");
    }
    // Static LLVM is C++, so the C++ runtime must come from somewhere; this
    // is the same choice `llvm-sys`'s own linking makes per platform.
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    match target_os.as_str() {
        "macos" | "freebsd" => println!("cargo:rustc-link-lib=dylib=c++"),
        "linux" => println!("cargo:rustc-link-lib=dylib=stdc++"),
        // MSVC links its C++ runtime through the object files themselves.
        _ => {}
    }
}

/// The workspace root, two levels above this crate's manifest.
fn repo_root() -> PathBuf {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    Path::new(&manifest)
        .join("../..")
        .canonicalize()
        .unwrap_or_else(|error| {
            fail(&format!(
                "cannot resolve the workspace root above `{manifest}`: {error}"
            ))
        })
}

/// Runs `llvm-config` with `args`, failing the build with its stderr on error.
fn run(llvm_config: &Path, args: &[&str]) -> String {
    let output = match Command::new(llvm_config).args(args).output() {
        Ok(output) => output,
        Err(error) => fail(&format!("cannot run `{}`: {error}", llvm_config.display())),
    };
    if !output.status.success() {
        fail(&format!(
            "`{} {}` failed: {}",
            llvm_config.display(),
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Library names out of an `llvm-config` link line.
///
/// Unix spells them `-lLLVMCore`; MSVC spells them `LLVMCore.lib`. Anything
/// else on the line (search-path flags, verbatim paths) is not a library
/// name and is dropped.
fn link_names(line: &str) -> Vec<String> {
    line.split_whitespace()
        .filter_map(|token| {
            if let Some(name) = token.strip_prefix("-l") {
                Some(name.to_owned())
            } else {
                token
                    .strip_suffix(".lib")
                    .map(|name| name.rsplit(['/', '\\']).next().unwrap_or(name).to_owned())
            }
        })
        .collect()
}

/// Fails the build with a message cargo shows the user, without a backtrace.
fn fail(message: &str) -> ! {
    eprintln!("kira-llvm-backend: {message}");
    std::process::exit(1);
}
