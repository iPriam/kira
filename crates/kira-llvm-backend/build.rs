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
    // The bundle's LLVM archives are linked statically: a `kira` must run
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

    declare_code_generators(&installation.home, &llvm_config);
}

/// Every code generator this crate knows how to register, paired with the cfg
/// that says the linked bundle actually defines it.
///
/// One row per generator rather than a special case per generator: the
/// registration in `codegen/target.rs` names exactly these cfgs, and a row
/// added here without a matching `#[cfg]` there is a generator that is present
/// and never registered — which reads at run time as "LLVM knows no such
/// triple" on a bundle that has the code.
const CODE_GENERATORS: &[(&str, &str)] = &[
    ("X86", "kira_llvm_x86"),
    ("AArch64", "kira_llvm_aarch64"),
    (kira_toolchain::WEB_CODE_GENERATOR, "kira_llvm_webassembly"),
];

/// Tells the crate which code generators the bundle it is linking defines.
///
/// The per-target initializers are real LLVM symbols living in that target's
/// archive, so naming one the bundle was not built with is a link failure —
/// four unresolved `LLVMInitializeWebAssembly*` symbols, at the end of a full
/// workspace build, naming nothing that could be acted on. What the bundle
/// carries is knowable before a single symbol is emitted: its own
/// `llvm-config` reports it, and one cfg per generator carries the answer into
/// the source. A bundle without the host's own generator is refused outright —
/// it can emit for nothing at all.
///
/// The generators that are not this host's are what `kira build --target` emits
/// through, and a bundle published before `llvm-metadata.toml` named them
/// outright carries only the one belonging to the runner that built it. That is
/// a warning rather than a failure: such a bundle still builds a perfectly good
/// compiler for its own host, and the missing generator is reported again, by
/// name, if a build actually asks for that target.
fn declare_code_generators(home: &Path, llvm_config: &Path) {
    for (_, cfg) in CODE_GENERATORS {
        println!("cargo::rustc-check-cfg=cfg({cfg})");
    }

    let built = match kira_toolchain::llvm_code_generators::built(llvm_config) {
        Ok(built) => built,
        Err(error) => fail(&error.to_string()),
    };
    let host = kira_toolchain::llvm_code_generators::host_code_generator().unwrap_or_else(|| {
        fail(&format!(
            "Kira has no LLVM code generator for {} hosts",
            std::env::consts::ARCH
        ))
    });
    if !built.iter().any(|name| name == host) {
        fail(&format!(
            "the managed LLVM at `{}` was built without the {host} code generator \
             (it carries: {}), so it cannot emit for this host",
            home.display(),
            built.join(", "),
        ));
    }

    for (generator, cfg) in CODE_GENERATORS {
        if built.iter().any(|name| name == generator) {
            println!("cargo:rustc-cfg={cfg}");
        } else {
            println!(
                "cargo:warning=the managed LLVM at {} was built without the \
                 {generator} code generator, so this compiler will refuse {}; the \
                 bundle predates the targets `llvm-metadata.toml` pins, and \
                 `knvm install-llvm --force` replaces it once they are published",
                home.display(),
                refusal(generator),
            );
        }
    }
}

/// What a compiler linked against a bundle missing `generator` cannot serve,
/// spelled as the command line that would ask for it.
fn refusal(generator: &str) -> String {
    if generator == kira_toolchain::WEB_CODE_GENERATOR {
        return "`--device wasm32`".to_owned();
    }
    format!("`--target` triples whose architecture needs {generator}")
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
