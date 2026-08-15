//! Calling C over a live session, on every live backend.
//!
//! A live session used to build no foreign surface at all: the bundle carried
//! bytecode and nothing else, so the first `@FFI.Extern` call in the runner's
//! process failed with "this host has no foreign-call binding loaded" after a
//! bundle that built, linked, and reported ready. Every app that talks to a
//! platform library is such a program, which is most of the apps a live session
//! exists for.
//!
//! The evidence here is the program's own output: values only the checked-in C
//! fixture computes, printed by a program running in the *runner's* process,
//! reached through direct Libffi bindings and dependencies the bundle carried
//! over a socket.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

/// What the fixture prints once every foreign call has gone through.
///
/// The final `1` and `2` are one C counter incremented twice, which is what
/// proves both calls reached the same loaded library rather than two copies.
const EXPECTED: &str = "42\n-5\n200\n-9\n40000\n4000000000\n1975\n5000000000\nfalse\n3.75\n1.75\n\
     4\n42\n0\n7\nhello from C\nround trip\n|\nhello from C!\n1\n2\n";

/// Output from the callback-state fixture after C enters Kira twice.
const STATE_EXPECTED: &str = "307\n2\nkept\n2\n7\n107\n";

/// A program, its C library, and the build tree they produce, removed on drop.
struct Fixture(PathBuf);

impl Fixture {
    /// Writes the Kira program and builds the C fixture into a static archive
    /// the package declares through `NativeLibs`.
    fn new(tag: &str, program: &str) -> Fixture {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "kira-live-foreign-{}-{tag}-{unique}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let lib = root.join("NativeLibs/lib");
        std::fs::create_dir_all(&lib).expect("native-lib directory");
        std::fs::write(root.join("main.kira"), program).expect("write program");
        std::fs::write(
            root.join("package.kira"),
            "Package LiveFfi {\n    let allowThinFfiShim = true\n}\n",
        )
        .expect("write package manifest");
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
[[target]]
triple = "x86_64-windows-msvc"
staticLib = "lib/libffifixture.a"
[[target]]
triple = "aarch64-windows-msvc"
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

/// A spawned `kira` process that is killed when it goes out of scope.
///
/// An unwatched session ends on its own, but only when nothing goes wrong: a
/// panic between the spawn and the wait leaves the process running, and `kira
/// live` supervises a runner of its own, so the survivor holds the inherited
/// pipe and the whole suite reads as leaking. Killing on drop turns either into
/// a failing test rather than a hanging one.
struct Session(Child);

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Runs one unwatched live session on `backend` and returns
/// (stdout, stderr, ok).
///
/// `--no-watch` in words: this reads the session's output to end of file, and
/// that only arrives when the session ends. A watched session is one that does
/// not end on its own, so it is the opposite of what is being read for.
fn live(fixture: &Fixture, backend: &str) -> (String, String, bool) {
    let mut session = Session(
        Command::new(env!("CARGO_BIN_EXE_kira"))
            .args(["live", "--no-watch", "--backend", backend])
            .arg(fixture.program())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("kira spawns"),
    );
    let child = &mut session.0;

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
/// The events carry backend-specific details and the port the OS handed out,
/// and neither is the program's behavior.
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
fn a_vm_live_session_calls_c_through_the_bundles_bindings() {
    let fixture = Fixture::new("vm", include_str!("fixtures/ffi/ffi_program.kira"));

    let (stdout, stderr, ok) = live(&fixture, "vm");

    assert!(ok, "the session must exit 0.\nstderr: {stderr}");
    assert_eq!(
        app_output(&stdout),
        expected_lines(),
        "every foreign call must produce the C fixture's own answer.\nstderr: {stderr}"
    );
}

/// The VM bundle carries separate foreign binding metadata and dependencies.
///
/// The event proves the bundle was built; the program output above proves the
/// runner loaded the bindings and their declared native dependencies. The exact
/// payload count is deliberately not part of the live contract.
#[test]
fn a_vm_bundle_with_foreign_imports_carries_binding_metadata() {
    let fixture = Fixture::new("payloads", include_str!("fixtures/ffi/ffi_program.kira"));

    let (stdout, stderr, ok) = live(&fixture, "vm");

    assert!(ok, "stderr: {stderr}");
    assert!(
        stdout
            .lines()
            .any(|line| line.starts_with("live.bundle.built ")),
        "the VM live bundle must be built before the runner starts.\nstdout: {stdout}\nstderr: {stderr}"
    );
}

/// VM, LLVM, and hybrid live sessions run the same foreign program to the same
/// answers.
///
/// The backend-parity promise is that where code runs does not change what it
/// does. VM reaches C through direct Libffi bindings, LLVM links the whole
/// native program, and hybrid splits the program and calls C from both halves.
#[test]
fn all_live_backends_agree_on_a_foreign_program() {
    let fixture = Fixture::new("parity", include_str!("fixtures/ffi/ffi_program.kira"));

    let (vm_stdout, vm_stderr, vm_ok) = live(&fixture, "vm");
    let (llvm_stdout, llvm_stderr, llvm_ok) = live(&fixture, "llvm");
    let (hybrid_stdout, hybrid_stderr, hybrid_ok) = live(&fixture, "hybrid");

    assert!(vm_ok, "the vm session failed.\nstderr: {vm_stderr}");
    assert!(llvm_ok, "the llvm session failed.\nstderr: {llvm_stderr}");
    assert!(
        hybrid_ok,
        "the hybrid session failed.\nstderr: {hybrid_stderr}"
    );
    assert_eq!(app_output(&vm_stdout), expected_lines());
    assert!(
        llvm_stdout
            .lines()
            .any(|line| line.starts_with("live.bundle.built ")),
        "the LLVM bundle must be built before the runner starts.\nstdout: {llvm_stdout}"
    );
    assert_eq!(app_output(&llvm_stdout), app_output(&vm_stdout));
    assert_eq!(app_output(&hybrid_stdout), app_output(&vm_stdout));
}

/// Every live backend preserves a user-defined state value across a C callback.
#[test]
fn all_live_backends_agree_on_callback_state_lifecycle() {
    let fixture = Fixture::new(
        "state-parity",
        include_str!("fixtures/ffi/ffi_program_state_callback.kira"),
    );

    let (vm_stdout, vm_stderr, vm_ok) = live(&fixture, "vm");
    let (llvm_stdout, llvm_stderr, llvm_ok) = live(&fixture, "llvm");
    let (hybrid_stdout, hybrid_stderr, hybrid_ok) = live(&fixture, "hybrid");

    assert!(vm_ok, "the vm session failed.\nstderr: {vm_stderr}");
    assert!(llvm_ok, "the llvm session failed.\nstderr: {llvm_stderr}");
    assert!(
        hybrid_ok,
        "the hybrid session failed.\nstderr: {hybrid_stderr}"
    );
    assert_eq!(app_output(&vm_stdout), expected_state_lines());
    assert_eq!(app_output(&llvm_stdout), app_output(&vm_stdout));
    assert_eq!(app_output(&hybrid_stdout), app_output(&vm_stdout));
}

/// The callback-state output as the lines a session prints.
fn expected_state_lines() -> Vec<String> {
    STATE_EXPECTED.lines().map(str::to_owned).collect()
}
