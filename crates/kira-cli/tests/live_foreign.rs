//! Calling C over a live session, on both live backends.
//!
//! A live session used to build no foreign surface at all: the bundle carried
//! bytecode and nothing else, so the first `@FFI.Extern` call in the runner's
//! process failed with "this host has no foreign-call adapter loaded" — after a
//! bundle that built, linked, and reported ready. Every app that talks to a
//! platform library is such a program, which is most of the apps a live session
//! exists for.
//!
//! The evidence here is the program's own output: values only the checked-in C
//! fixture computes, printed by a program running in the *runner's* process,
//! reached through adapters the bundle carried over a socket.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

/// What the fixture prints once every foreign call has gone through.
///
/// The final `1` and `2` are one C counter incremented twice, which is what
/// proves both calls reached the same loaded library rather than two copies.
const EXPECTED: &str = "42\n-5\n200\n-9\n40000\n4000000000\n1975\n5000000000\nfalse\n3.75\n1.75\n\
     4\n42\n0\n7\nhello from C\nround trip\n|\nhello from C!\n1\n2\n";

/// A program, its C library, and the build tree they produce, removed on drop.
struct Fixture(PathBuf);

impl Fixture {
    /// Writes the Kira program and builds the C fixture into a static archive
    /// the package declares through `NativeLibs`.
    fn new(tag: &str) -> Fixture {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "kira-live-foreign-{}-{tag}-{unique}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let lib = root.join("NativeLibs/lib");
        std::fs::create_dir_all(&lib).expect("native-lib directory");
        std::fs::write(
            root.join("main.kira"),
            include_str!("fixtures/ffi/ffi_program.kira"),
        )
        .expect("write program");
        std::fs::write(root.join("NativeLibs/ffifixture.toml"), HOST_MANIFEST)
            .expect("write native-library manifest");
        std::fs::write(
            lib.join("ffi_fixture.c"),
            include_str!("fixtures/ffi/ffi_fixture.c"),
        )
        .expect("write fixture source");
        std::fs::write(
            lib.join("ffi_fixture.h"),
            include_str!("fixtures/ffi/ffi_fixture.h"),
        )
        .expect("write fixture header");
        build_archive(&lib);
        Fixture(root)
    }

    fn program(&self) -> PathBuf {
        self.0.join("main.kira")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The host triples the one built archive answers for.
const HOST_MANIFEST: &str = r#"name = "ffifixture"
[[target]]
triple = "aarch64-macos-none"
staticLib = "lib/libffifixture.a"
[[target]]
triple = "x86_64-macos-none"
staticLib = "lib/libffifixture.a"
[[target]]
triple = "x86_64-linux-gnu"
staticLib = "lib/libffifixture.a"
[[target]]
triple = "aarch64-linux-gnu"
staticLib = "lib/libffifixture.a"
"#;

/// Compiles the C fixture into `libffifixture.a` with the managed LLVM.
fn build_archive(lib: &Path) {
    let llvm = kira_toolchain::discover(None).expect("the managed LLVM is present");
    let object = lib.join("ffi_fixture.o");
    let compile = Command::new(llvm.clang())
        .arg("-c")
        .arg(lib.join("ffi_fixture.c"))
        .arg("-o")
        .arg(&object)
        .arg("-I")
        .arg(lib)
        .output()
        .expect("clang runs");
    assert!(
        compile.status.success(),
        "compiling the C fixture failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let archive = Command::new(llvm.llvm_ar())
        .arg("crs")
        .arg(lib.join("libffifixture.a"))
        .arg(&object)
        .output()
        .expect("llvm-ar runs");
    assert!(
        archive.status.success(),
        "archiving the C fixture failed: {}",
        String::from_utf8_lossy(&archive.stderr)
    );
}

/// Runs one unwatched live session on `backend` and returns
/// (stdout, stderr, ok).
fn live(fixture: &Fixture, backend: &str) -> (String, String, bool) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_kira"))
        .args(["live", "--backend", backend])
        .arg(fixture.program())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("kira spawns");

    let mut stdout = String::new();
    child
        .stdout
        .take()
        .expect("stdout is piped")
        .read_to_string(&mut stdout)
        .expect("read stdout");
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .expect("stderr is piped")
        .read_to_string(&mut stderr)
        .expect("read stderr");
    let status = child.wait().expect("kira exits");
    (stdout, stderr, status.success())
}

/// The app's own lines, with the session's events filtered out.
///
/// The events differ by payload count and by the port the OS handed out, and
/// neither is the program's behavior.
fn app_output(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .filter(|line| !line.starts_with("live."))
        .map(str::to_owned)
        .collect()
}

/// The expected output as the lines a session prints.
fn expected_lines() -> Vec<String> {
    EXPECTED.lines().map(str::to_owned).collect()
}

/// A VM live session reaches C.
#[test]
fn a_vm_live_session_calls_c_through_the_bundles_adapters() {
    let fixture = Fixture::new("vm");

    let (stdout, stderr, ok) = live(&fixture, "vm");

    assert!(ok, "the session must exit 0.\nstderr: {stderr}");
    assert_eq!(
        app_output(&stdout),
        expected_lines(),
        "every foreign call must produce the C fixture's own answer.\nstderr: {stderr}"
    );
}

/// The bundle carries the native half the adapters live in.
///
/// One payload means the session went back to shipping bytecode alone, which is
/// exactly the state that made a foreign call fail in the runner: the count is
/// what turns that regression into a failing test rather than a trap at
/// someone's entrypoint.
#[test]
fn a_vm_bundle_with_foreign_imports_carries_a_native_half() {
    let fixture = Fixture::new("payloads");

    let (stdout, stderr, ok) = live(&fixture, "vm");

    assert!(ok, "stderr: {stderr}");
    assert!(
        stdout.contains("live.bundle.built payloads=3"),
        "a VM bundle that reaches C is a manifest, the bytecode, and the \
         adapters' library.\nstdout: {stdout}\nstderr: {stderr}"
    );
}

/// Both live backends run the same foreign program to the same answers.
///
/// The dual-mode promise is that where code runs does not change what it does,
/// and the two backends reach C differently: the VM bundle puts every function
/// on the VM and the native half holds only adapters, while the hybrid bundle
/// splits the program and calls C from both halves.
#[test]
fn both_live_backends_agree_on_a_foreign_program() {
    let fixture = Fixture::new("parity");

    let (vm_stdout, vm_stderr, vm_ok) = live(&fixture, "vm");
    let (hybrid_stdout, hybrid_stderr, hybrid_ok) = live(&fixture, "hybrid");

    assert!(vm_ok, "the vm session failed.\nstderr: {vm_stderr}");
    assert!(
        hybrid_ok,
        "the hybrid session failed.\nstderr: {hybrid_stderr}"
    );
    assert_eq!(app_output(&vm_stdout), expected_lines());
    assert_eq!(app_output(&hybrid_stdout), app_output(&vm_stdout));
}
