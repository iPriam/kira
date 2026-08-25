//! A standalone hybrid program, end to end.
//!
//! What it proves is not that the backend can compile both halves — the VM
//! corpus covers execution and the live tests cover reload. It is that
//! `kira build --backend hybrid` leaves behind something the operating system
//! starts: an executable which, moved somewhere else with its three payload
//! files and nothing else, still runs and prints what only the two engines
//! together can compute.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A scratch directory that removes itself, holding a program and its build.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let path = std::env::temp_dir().join(format!(
            "kira-hybrid-standalone-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("scratch dir");
        Scratch(path)
    }

    fn write_program(&self, source: &str) -> PathBuf {
        let path = self.0.join("app.kira");
        std::fs::write(&path, source).expect("write program");
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Runs `kira build --backend hybrid` on `path`.
fn build_hybrid(path: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_kira"))
        .args(["build", "--backend", "hybrid"])
        .arg(path)
        .output()
        .expect("spawn kira")
}

/// Runs the staged executable from a working directory nowhere near the bundle.
fn run_standalone(executable: &Path) -> Output {
    Command::new(executable)
        .current_dir(std::env::temp_dir())
        .output()
        .expect("spawn the staged executable")
}

/// The same four files, deployed somewhere else entirely, under a new name.
///
/// A deployment story that only works inside `.kira-build` is not one: the
/// launcher resolves its manifest beside itself, so a directory holding the
/// renamed executable and the three payloads runs wherever it lands.
#[test]
fn a_built_hybrid_program_runs_standalone_and_relocates() {
    let scratch = Scratch::new("relocate");
    let program = scratch.write_program(include_str!("fixtures/live/hybrid_native.kira"));

    let build = build_hybrid(&program);
    let stdout = String::from_utf8_lossy(&build.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&build.stderr).into_owned();
    assert!(
        build.status.success(),
        "build failed:\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(stdout.contains("Successfully built"), "{stdout}");

    // The staged executable is named after the program, like a native build's.
    let executable = if cfg!(target_os = "windows") {
        scratch.0.join(".kira-build").join("app.exe")
    } else {
        scratch.0.join(".kira-build").join("app")
    };
    assert!(
        executable.is_file(),
        "{} was not staged",
        executable.display()
    );

    let run = run_standalone(&executable);
    let stdout = String::from_utf8_lossy(&run.stdout).into_owned();
    assert!(
        run.status.success(),
        "standalone run failed:\nstdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    // One value only the native half computes, one only through the bridge.
    assert_eq!(stdout, "84\n21\n");

    // Relocate: every file the build wrote, into a directory with no relation
    // to the build tree, under an executable name the build never used.
    let deployed = scratch.0.join("deployed");
    std::fs::create_dir_all(&deployed).expect("deployment directory");
    let deployed_executable = deployed.join(if cfg!(target_os = "windows") {
        "shipped.exe"
    } else {
        "shipped"
    });
    std::fs::copy(&executable, &deployed_executable).expect("stage the deployment");
    // Every payload the bundle needs, minus what only a rebuild consumes.
    for entry in std::fs::read_dir(scratch.0.join(".kira-build")).expect("build directory") {
        let entry = entry.expect("build directory entry");
        let name = entry.file_name();
        let name = name.to_string_lossy().into_owned();
        let disposable = name == ".build-lock"
            || name.ends_with(".o")
            || name.ends_with(".ll")
            || name.ends_with(".native-surface")
            || name
                == if cfg!(target_os = "windows") {
                    "app.exe"
                } else {
                    "app"
                };
        if !disposable {
            std::fs::copy(entry.path(), deployed.join(&name)).expect("copy payload");
        }
    }

    let relocated = Command::new(&deployed_executable)
        .current_dir(std::env::temp_dir())
        .output()
        .expect("spawn the deployed executable");
    assert!(relocated.status.success());
    assert_eq!(String::from_utf8_lossy(&relocated.stdout), "84\n21\n");
}
