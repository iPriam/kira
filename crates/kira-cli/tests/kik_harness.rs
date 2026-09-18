//! The in-depth stress harness, run through the real binary.
//!
//! `tests-kik/harness` is one Kira package that exercises the executable
//! surface in depth: fifteen areas, over a thousand `Test` declarations, and a
//! `@Main` that reduces every area to a checksum. It is the corpus the
//! reference implementation used as its own stress suite, ported here as Kira
//! source — behavior, never internals.
//!
//! Two things are gated, and they catch different failures. The suite catches a
//! *wrong value*: a case computes something and asserts what it should be. The
//! checksum run catches a *backend divergence*: the same program printing
//! different bytes on two engines is a parity bug even when every case passes,
//! because a case only looks at what it thought to look at.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The harness package, relative to this crate.
fn harness() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests-kik/harness")
}

/// Runs the shipped binary against the harness with this checkout's Foundation.
///
/// Pinned rather than discovered: the harness exercises its own test runner,
/// and an installed toolchain's Foundation is not the test package's source.
fn kira(args: &[&str]) -> std::process::Output {
    let foundation = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../foundation")
        .canonicalize()
        .expect("the checkout's foundation");
    Command::new(env!("CARGO_BIN_EXE_kira"))
        .env("KIRA_FOUNDATION_HOME", foundation)
        .args(args)
        .output()
        .expect("run kira")
}

/// The FFI harness package, relative to this crate.
///
/// A second suite beside the first, and hybrid-only for a reason: every case in
/// it mixes `@Native` and `@Runtime`, which pure vm and pure llvm refuse. What
/// it exercises is the seam itself — a struct returned by value from native
/// code, an enum crossing into a VM closure, an array written through a
/// `borrow mut` parameter and read back on the other side — which no
/// single-engine suite reaches, because on one engine there is no crossing.
fn ffi_harness() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests-kik/ffi-harness")
}

/// The lifecycle entry harness, separate because an application has one entry.
fn main_thread_lifecycle_harness() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests-kik/main-thread-lifecycle")
}

/// A package under `tests-kik`, by directory name.
fn kik(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests-kik")
        .join(name)
}

/// A macro is a name a file writes, so an import is what makes it nameable.
///
/// The allowed half of the rule, as a program: every file that calls the
/// dependency's macro imports the package that declares it, one of them under
/// an alias. The refused half cannot be a running program at all, so it lives
/// in the package below.
#[test]
fn grouped_extern_library_runs_on_every_backend() {
    let path = kik("grouped-ffi");
    let path = path.to_str().expect("a utf-8 path");
    for backend in ["vm", "llvm", "hybrid"] {
        let output = kira(&["run", "--backend", backend, path]);
        assert!(
            output.status.success(),
            "the grouped extern harness failed on {backend}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "true\n",
            "the grouped extern call diverged on {backend}"
        );
    }
}

#[test]
fn a_macro_is_reachable_through_the_import_that_names_its_package() {
    let path = kik("macro-imports");
    let path = path.to_str().expect("a utf-8 path");
    for backend in ["vm", "llvm", "hybrid"] {
        let output = kira(&["run", "--backend", backend, path]);
        assert!(
            output.status.success(),
            "the macro import harness failed on {backend}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "42\n8\n26\n",
            "the macro import harness diverged on {backend}"
        );
    }
}

/// One expected-or-reported refusal: a diagnostic code anchored at a source
/// line, keyed by the file's path relative to the refusals package.
///
/// The whole comparison is over multisets of these — a rule that stopped
/// applying to three of four nested shapes drops one entry, a rule that fired
/// where it should not adds one, and either way the sorted vectors differ.
type Refusal = (String, usize, String);

/// The refusal each source line marks with a `//~ CODE …` comment.
///
/// A compiletest-style annotation: the codes a line names are the diagnostics
/// the build must anchor at that same line, so the expectation lives beside the
/// code it is about rather than as a count in this file. One comment may list
/// several codes when a single line earns several refusals.
fn annotated_refusals(root: &Path) -> Vec<Refusal> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<Refusal>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .expect("read a refusals directory")
            .map(|e| e.expect("a refusals entry").path())
            .collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                walk(&path, root, out);
            } else if path.extension().is_some_and(|e| e == "kira") {
                let rel = path
                    .strip_prefix(root)
                    .expect("a path under the refusals root")
                    .to_string_lossy()
                    .replace('\\', "/");
                let text = std::fs::read_to_string(&path).expect("read a refusal source");
                for (index, line) in text.lines().enumerate() {
                    let Some((_, marked)) = line.split_once("//~") else {
                        continue;
                    };
                    for code in marked.split_whitespace() {
                        out.push((rel.clone(), index + 1, code.to_owned()));
                    }
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

/// Every refusal the build reported, parsed from its `error[CODE]` headers and
/// the `--> path:line:col` line that locates each one.
///
/// Paths are normalised to the same package-relative form the annotations use,
/// so the two multisets are directly comparable.
fn reported_refusals(output: &str) -> Vec<Refusal> {
    let mut out = Vec::new();
    let mut pending: Option<String> = None;
    for line in output.lines() {
        if let Some(rest) = line.trim_start().strip_prefix("error[") {
            if let Some(code) = rest.split(']').next() {
                pending = Some(code.to_owned());
            }
        } else if let (Some(code), Some(pos)) = (pending.clone(), line.find("-->")) {
            let located = line[pos + 3..].trim();
            let mut parts = located.rsplitn(3, ':');
            let (_col, row, file) = (parts.next(), parts.next(), parts.next());
            if let (Some(row), Some(file)) = (row, file) {
                if let (Ok(row), Some(idx)) =
                    (row.parse::<usize>(), file.find("refusals/"))
                {
                    let rel = file[idx + "refusals/".len()..].to_owned();
                    out.push((rel, row, code));
                    pending = None;
                }
            }
        }
    }
    out.sort();
    out
}

/// The programs the language refuses, and the codes it refuses them under.
///
/// A harness of runnable cases cannot hold a refusal — the point of each one is
/// that it never becomes a program — so they are one package that must not
/// build. Each refusal is pinned by a `//~ CODE` comment on the line it fires
/// at, and the build's reported diagnostics must be exactly that set: no rule
/// that quietly stopped firing, and no new refusal that crept in unannotated.
#[test]
fn the_refusal_harness_refuses_every_program_in_it() {
    let root = kik("refusals");
    let path = root.to_str().expect("a utf-8 path");
    let output = kira(&["build", "--backend", "vm", path]);
    assert!(
        !output.status.success(),
        "the refusal harness built, so nothing in it is being refused"
    );
    let reported = String::from_utf8_lossy(&output.stderr).into_owned()
        + &String::from_utf8_lossy(&output.stdout);

    let expected = annotated_refusals(&root);
    let actual = reported_refusals(&reported);
    let missing: Vec<_> = expected.iter().filter(|r| !actual.contains(r)).collect();
    let unexpected: Vec<_> = actual.iter().filter(|r| !expected.contains(r)).collect();
    assert!(
        missing.is_empty() && unexpected.is_empty(),
        "the refusals reported do not match their `//~` annotations:\n  \
         annotated but not reported: {missing:?}\n  \
         reported but not annotated: {unexpected:?}\n\nfull output:\n{reported}"
    );
}

/// A run that ends with payloads still queued releases the storage they name.
///
/// Not a `Test` construct, and it could not be one: the program prints the same
/// bytes and exits zero whether the storage came back or not. The runtime's own
/// heap balance is the only thing that can tell, so it is what is asserted —
/// and the allocation count with it, because a case that allocated nothing
/// balances trivially and proves nothing.
#[test]
fn a_run_that_ends_with_payloads_queued_leaves_the_native_heap_balanced() {
    let path = kik("channel-teardown");
    let path = path.to_str().expect("a utf-8 path");
    for backend in ["vm", "llvm", "hybrid"] {
        let output = Command::new(env!("CARGO_BIN_EXE_kira"))
            .env(
                "KIRA_FOUNDATION_HOME",
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../foundation")
                    .canonicalize()
                    .expect("the checkout's foundation"),
            )
            .env("KIRA_HEAP_REPORT", "1")
            .args(["run", "--backend", backend, path])
            .output()
            .expect("run kira");
        assert!(
            output.status.success(),
            "the teardown harness failed on {backend}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "1\n1\n1\none\ntwo\n",
            "the teardown harness diverged on {backend}"
        );
        let reported = String::from_utf8_lossy(&output.stderr).into_owned();
        if backend == "vm" {
            // The VM keeps its own heap and reports no native counters; what it
            // contributes is that the same program means the same thing there.
            continue;
        }
        assert!(
            reported.contains("imbalance=+0"),
            "the native heap did not balance on {backend}:\n{reported}"
        );
    }
    // The engine that does the allocating for this program is the native one,
    // so it is the one that has to show the case is not balancing trivially.
    let output = Command::new(env!("CARGO_BIN_EXE_kira"))
        .env(
            "KIRA_FOUNDATION_HOME",
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../foundation")
                .canonicalize()
                .expect("the checkout's foundation"),
        )
        .env("KIRA_HEAP_REPORT", "1")
        .args(["run", "--backend", "llvm", path])
        .output()
        .expect("run kira");
    let reported = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        !reported.contains("allocated=0"),
        "the teardown case allocated nothing, so its balance proves nothing:\n{reported}"
    );
}

#[test]
fn main_thread_lifecycle_runs_across_every_executable_backend() {
    let path = main_thread_lifecycle_harness();
    let path = path.to_str().expect("a utf-8 path");
    for backend in ["vm", "llvm", "hybrid"] {
        let output = kira(&["run", "--backend", backend, path]);
        assert!(
            output.status.success(),
            "the lifecycle harness failed on {backend}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "main-thread-lifecycle\n42\nmanual-main-thread\n20021\n",
            "the lifecycle call tree diverged on {backend}"
        );
    }
}

/// Every case in the FFI harness passes on the hybrid engine.
///
/// The tally is asserted whole, as the suite above does, so a file that stops
/// being compiled fails this rather than quietly shrinking the run.
#[test]
fn the_ffi_harness_passes_on_the_hybrid_engine() {
    let path = ffi_harness();
    let path = path.to_str().expect("a utf-8 path");
    let output = kira(&["test", "--backend", "hybrid", path]);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "the ffi harness did not run: {}\n{stdout}",
        String::from_utf8_lossy(&output.stderr)
    );
    let tally = stdout.lines().last().unwrap_or_default().to_owned();
    assert!(
        tally.contains("0 failed"),
        "the ffi harness reported failures: {tally}"
    );
    assert_eq!(
        tally, "308 passed, 0 failed, 0 skipped, 308 total",
        "the ffi harness tally changed"
    );
}

/// Every case in the LibTessera harness passes on every executable backend.
///
/// `tests-kik/tessera-harness` drives `packages/tessera`, the portable front of
/// the system interface. The front is kernel-independent library state, so
/// unlike the syscall harness it is not gated on an operating system and must
/// pass identically on `vm`, `llvm` and `hybrid`. The tally is asserted whole so
/// a case that stops being collected fails this rather than shrinking the run.
/// Import-boundary visibility: an app names an imported package's `public`
/// declarations and cannot name its private ones.
///
/// The positive half runs on every backend — visibility is settled during name
/// resolution, before any backend sees the program, so the three must agree —
/// and the negative half is a build that must fail because the private helper
/// is not in scope across the import.
#[test]
fn public_crosses_a_package_import_and_private_does_not() {
    let path = kik("visibility");
    let path = path.to_str().expect("a utf-8 path");
    for backend in ["vm", "llvm", "hybrid"] {
        let output = kira(&["run", "--backend", backend, path]);
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "the visibility harness failed on {backend}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            stdout.contains("vis:7"),
            "public access diverged on {backend}: {stdout}"
        );
    }
    let refuse = kik("visibility/refuse");
    let output = kira(&["build", "--backend", "vm", refuse.to_str().expect("a utf-8 path")]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "a program naming a private declaration across an import built anyway"
    );
    assert!(
        stderr.contains("visInternalDouble"),
        "the refusal did not name the private declaration: {stderr}"
    );
}

/// Structural ordering walks an aggregate the way `==` does, with no `@Derive`,
/// and the walk is byte-identical on every backend.
///
/// A struct orders field-by-field, an array element-by-element then by length,
/// a string by its bytes, an enum by tag then payload — down to an enum whose
/// payload is itself a struct — and a `<T: Ordered>` bound accepts any of them by
/// shape. The native walk (`kira_rt_str_cmp`, `kira_rt_array_cmp`, `kira_rt_any_cmp`,
/// a struct `Cmp` leaf, and the payload cmp leaf on an aggregate enum box) mirrors
/// the VM's `Heap::compare_values`, so the same program prints the same bytes on
/// the vm, llvm, and hybrid — asserted whole, because a divergence is exactly the
/// failure this feature has to not have.
#[test]
fn structural_ordering_is_byte_identical_across_backends() {
    let path = kik("ordering");
    let path = path.to_str().expect("a utf-8 path");
    const EXPECTED: &str = "true\nfalse\ntrue\ntrue\ntrue\ntrue\nfalse\ntrue\ntrue\ntrue\ntrue\n\
         true\nfalse\ntrue\ntrue\ntrue\nfalse\ntrue\ntrue\ntrue\n";
    for backend in ["vm", "llvm", "hybrid"] {
        let output = kira(&["run", "--backend", backend, path]);
        assert!(
            output.status.success(),
            "the ordering harness failed on {backend}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            EXPECTED,
            "structural ordering diverged on {backend}"
        );
    }
}

/// Structural `hash(v)` folds a value byte-identically on every backend.
///
/// `hash` is the native operation behind the `Hashable` trait — the fold twin of
/// `==`, earned by any type whose leaves hash consistently with equality. The
/// native walk (a `Hash` element leaf and runtime FNV helpers) mirrors the VM's
/// `Heap::hash_value`, so a struct, an array, a string, a scalar, or a
/// payloadless enum folds to the same number on the vm, llvm, and hybrid — and
/// equal values hash equal, the contract with `==`. Asserted whole, because a
/// divergent hash is exactly the failure this must not have.
#[test]
fn hashing_is_byte_identical_across_backends() {
    let path = kik("hashable");
    let path = path.to_str().expect("a utf-8 path");
    const EXPECTED: &str =
        "true\ntrue\ntrue\ntrue\nfalse\nfalse\nfalse\ntrue\nfalse\ntrue\nfalse\ntrue\n";
    for backend in ["vm", "llvm", "hybrid"] {
        let output = kira(&["run", "--backend", backend, path]);
        assert!(
            output.status.success(),
            "the hashable harness failed on {backend}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            EXPECTED,
            "structural hashing diverged on {backend}"
        );
    }
}

/// The whole stress harness leaves the native heap balanced — no leak, no
/// double free — on every engine that keeps a native heap.
///
/// The suite's own case assertions check *values*; this checks the invariant no
/// case can see from inside itself. The allocation count is asserted non-trivial
/// alongside the balance, because a run that allocated nothing balances for the
/// wrong reason.
#[test]
fn the_harness_leaves_the_native_heap_balanced() {
    let path = harness();
    let path = path.to_str().expect("a utf-8 path");
    for backend in ["llvm", "hybrid"] {
        let output = Command::new(env!("CARGO_BIN_EXE_kira"))
            .env(
                "KIRA_FOUNDATION_HOME",
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../foundation")
                    .canonicalize()
                    .expect("the checkout's foundation"),
            )
            .env("KIRA_HEAP_REPORT", "1")
            .args(["run", "--backend", backend, path])
            .output()
            .expect("run kira");
        assert!(
            output.status.success(),
            "the harness failed on {backend}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let reported = String::from_utf8_lossy(&output.stderr).into_owned();
        assert!(
            reported.contains("imbalance=+0"),
            "the native heap did not balance on {backend}:\n{reported}"
        );
        assert!(
            !reported.contains("allocated=0 "),
            "the harness allocated nothing on {backend}, so its balance proves nothing:\n{reported}"
        );
    }
}

/// The whole stress harness runs clean under AddressSanitizer on the native
/// backend — no use-after-free, no overflow, no leak the allocator can see.
///
/// A second engine on the same invariant as the heap-balance gate, from the
/// outside: the balance counters are the runtime's own accounting, and this is
/// the toolchain's, so a bug that fooled one is unlikely to fool both.
#[test]
fn the_harness_is_clean_under_address_sanitizer() {
    let path = harness();
    let path = path.to_str().expect("a utf-8 path");
    let output = Command::new(env!("CARGO_BIN_EXE_kira"))
        .env(
            "KIRA_FOUNDATION_HOME",
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../foundation")
                .canonicalize()
                .expect("the checkout's foundation"),
        )
        .args(["run", "--backend", "llvm", "--sanitize", "address", path])
        .output()
        .expect("run kira");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("ERROR: AddressSanitizer"),
        "AddressSanitizer reported a fault:\n{stderr}"
    );
    assert!(
        output.status.success(),
        "the harness did not run clean under AddressSanitizer:\n{stderr}"
    );
}

/// `assert` / `assertEqual` pass silently on true conditions and equal values,
/// and a failing `assertEqual` hard-traps through `abort` on every backend.
///
/// The positive run proves the pass path — including structural `assertEqual`
/// of a struct, a payload enum, and an array — is identical on all three
/// engines; the negative run proves a failed assertion emits its message and
/// exits non-zero (the child-runner trap), with nothing printed after it, and
/// with the same bytes across engines.
#[test]
fn assertions_pass_and_a_failing_one_traps_on_every_backend() {
    let path = kik("assertions");
    let path = path.to_str().expect("a utf-8 path");
    for backend in ["vm", "llvm", "hybrid"] {
        let output = kira(&["run", "--backend", backend, path]);
        assert!(
            output.status.success(),
            "the assertions harness failed on {backend}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("assertions-passed"),
            "an assertion that should hold did not on {backend}"
        );
    }
    let abort = kik("assertions/abort");
    let abort = abort.to_str().expect("a utf-8 path");
    for backend in ["vm", "llvm", "hybrid"] {
        let output = kira(&["run", "--backend", backend, abort]);
        assert!(
            !output.status.success(),
            "a failing assertion did not trap on {backend}"
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("before-abort") && !stdout.contains("after-abort"),
            "abort did not stop execution on {backend}: {stdout}"
        );
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("aborted: assertion failed"),
            "the abort message is missing on {backend}"
        );
    }
}

#[test]
fn the_tessera_harness_passes_on_every_backend() {
    for backend in ["vm", "llvm", "hybrid"] {
        let path = kik("tessera-harness");
        let path = path.to_str().expect("a utf-8 path");
        let output = kira(&["test", "--backend", backend, path]);
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        assert!(
            output.status.success(),
            "the tessera harness did not run on {backend}: {}\n{stdout}",
            String::from_utf8_lossy(&output.stderr)
        );
        let tally = stdout.lines().last().unwrap_or_default().to_owned();
        assert_eq!(
            tally, "26 passed, 0 failed, 0 skipped, 26 total",
            "the tessera harness tally changed on {backend}"
        );
    }
}

/// The raw system-call harness package, relative to this crate.
///
/// Linux-only, and that is not a choice: a `@FFI.Syscall` is refused at compile
/// time on a target that cannot reach the Linux kernel — that refusal is the
/// feature — so on another operating system there is no program here to run.
///
/// This package holds the calls that only an emitted instruction can make;
/// [`syscall_parity_harness`] holds the ones a host can make for an interpreted
/// program. The split is what the VM's refusal forces, and both READMEs say so.
#[cfg(target_os = "linux")]
fn syscall_harness() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests-kik/syscall-harness")
}

/// Every case in the system-call harness passes against a real kernel, on both
/// engines that emit the instruction.
///
/// Both are asserted rather than one, because they reach the kernel by different
/// routes and only running both proves the second. On `llvm` the call site is
/// machine code in the program. On `hybrid` the bodies holding the calls are the
/// native half and a `Test` reaches them across the bridge, which is the same
/// crossing the FFI harness exercises — so a change that broke the native half's
/// lowering while leaving a whole-program build working would fail here.
///
/// Gated on the operating system rather than skipped inside the suite, and the
/// tally asserted whole for the same reason the others are: a case that stops
/// being collected fails this rather than quietly shrinking the run, which is
/// exactly what a bodyless declaration in the same file used to cause.
#[test]
#[cfg(target_os = "linux")]
fn the_syscall_harness_passes_against_the_kernel() {
    for backend in ["llvm", "hybrid"] {
        let path = syscall_harness();
        let path = path.to_str().expect("a utf-8 path");
        let output = kira(&["test", "--backend", backend, path]);
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        assert!(
            output.status.success(),
            "the syscall harness did not run on {backend}: {}\n{stdout}",
            String::from_utf8_lossy(&output.stderr)
        );
        let tally = stdout.lines().last().unwrap_or_default().to_owned();
        assert_eq!(
            tally, "19 passed, 0 failed, 0 skipped, 19 total",
            "the syscall harness tally changed on {backend}"
        );
    }
}

/// The servable-system-call package, relative to this crate.
///
/// The other half of the suite above, and Linux-only for the same reason. It
/// declares only the four calls an interpreter can serve, which is what lets one
/// source run on all three engines.
#[cfg(target_os = "linux")]
fn syscall_parity_harness() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests-kik/syscall-parity-harness")
}

/// Every case in the servable-system-call package passes on all three engines.
///
/// The leg the suite above cannot have. `syscall-harness` names `mount`,
/// `execve`, `wait4` and `umount2`, and the VM refuses a program that names one
/// before it starts — so it can never run there, and its lowering had no
/// interpreter to be checked against. This package makes only the calls a host
/// can make on a program's behalf, so the interpreter and the emitted
/// instruction can be asked the same question.
///
/// The tally is asserted whole, as every harness here does: a case that stops
/// being collected fails this rather than quietly shrinking the run.
#[test]
#[cfg(target_os = "linux")]
fn the_servable_syscall_harness_passes_on_every_engine() {
    for backend in ["vm", "llvm", "hybrid"] {
        let path = syscall_parity_harness();
        let path = path.to_str().expect("a utf-8 path");
        let output = kira(&["test", "--backend", backend, path]);
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        assert!(
            output.status.success(),
            "the servable syscall harness did not run on {backend}: {}\n{stdout}",
            String::from_utf8_lossy(&output.stderr)
        );
        let tally = stdout.lines().last().unwrap_or_default().to_owned();
        assert_eq!(
            tally, "9 passed, 0 failed, 0 skipped, 9 total",
            "the servable syscall harness tally changed on {backend}"
        );
    }
}

/// The same kernel answers print the same bytes on all three engines.
///
/// The half a passing suite cannot prove, for the one feature that had no such
/// check at all. Each printed number is derived from a real `-errno` the kernel
/// put in a register, so a host that decoded an answer the emitted call leaves
/// raw — or sign-extended a narrow descriptor differently — changes a line here
/// even when every case still passes.
#[test]
#[cfg(target_os = "linux")]
fn the_servable_syscall_harness_prints_the_same_bytes_on_every_engine() {
    let path = syscall_parity_harness();
    let path = path.to_str().expect("a utf-8 path");
    let runs: Vec<(&str, String)> = ["vm", "llvm", "hybrid"]
        .into_iter()
        .map(|backend| {
            let output = kira(&["run", "--backend", backend, path]);
            assert!(
                output.status.success(),
                "the servable syscall run failed on {backend}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            (
                backend,
                String::from_utf8_lossy(&output.stdout).into_owned(),
            )
        })
        .collect();
    assert!(
        runs[0].1.contains("kik-syscall-parity-end"),
        "the run did not finish: {}",
        runs[0].1
    );
    for (backend, stdout) in &runs[1..] {
        assert_eq!(
            &runs[0].1, stdout,
            "the vm and {backend} backends disagree on the kernel's answers"
        );
    }
}

/// The VM refuses a program naming a call no interpreter can serve, by name and
/// with the reason, before it starts.
///
/// Two halves, and both matter. It refuses, because `syscall-harness` names
/// seven calls that act on the interpreter's process or on the machine — and it
/// names each of them with what it would have done, because "the VM cannot do
/// this" leaves the reader to guess whether the fix is the program or the
/// command line. It does *not* name `write`, `read` or `ppoll`: those are served
/// now, and a refusal listing them would send an author to change a call that
/// works.
///
/// `sync` is asserted on the refused side rather than the served one, which is
/// the assertion this test exists to have. It takes no descriptor — it flushes
/// every filesystem on the machine — so serving it under the interpreter acts on
/// the developer's box, and on a 9p mount it does so uninterruptibly.
#[test]
#[cfg(target_os = "linux")]
fn the_vm_refuses_only_the_calls_no_interpreter_can_serve() {
    let path = syscall_harness();
    let path = path.to_str().expect("a utf-8 path");
    let output = kira(&["run", "--backend", "vm", path]);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(!output.status.success(), "the VM ran it anyway: {stderr}");
    assert!(
        stderr.contains("calls the Linux kernel directly"),
        "unexpected refusal: {stderr}"
    );
    assert!(
        stderr.contains("the process is the interpreter rather than the program"),
        "the refusal does not say why: {stderr}"
    );
    for call in [
        "sync",
        "mount",
        "umount2",
        "reboot",
        "execve",
        "wait4",
        "exit_group",
    ] {
        assert!(
            stderr.contains(&format!("`{call}`")),
            "`{call}` is not named: {stderr}"
        );
    }
    for served in ["`write`", "`read`", "`ppoll`"] {
        assert!(
            !stderr.contains(served),
            "{served} is served on the VM and must not be refused: {stderr}"
        );
    }
    assert!(
        stderr.contains("--backend llvm") && stderr.contains("--backend hybrid"),
        "the engines that do work are not both named: {stderr}"
    );
}

/// The last line of a run, which is the suite's tally.
fn tally(backend: &str) -> String {
    let path = harness();
    let path = path.to_str().expect("a utf-8 path");
    let output = kira(&["test", "--backend", backend, path]);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "the harness did not run on {backend}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    stdout.lines().last().unwrap_or_default().to_owned()
}

/// Every case that runs, passes — on the VM and on native alike, with the two
/// agreeing on the count.
///
/// The tally is compared as a whole rather than the failure count alone, so a
/// case that stops being *collected* fails this too. A suite that silently
/// shrinks is the failure mode a "zero failures" assertion cannot see.
#[test]
fn the_harness_suite_passes_identically_on_vm_and_native() {
    let vm = tally("vm");
    let llvm = tally("llvm");
    assert_eq!(vm, "1596 passed, 0 failed, 0 skipped, 1596 total");
    assert_eq!(vm, llvm, "the vm and native backends disagree on the suite");
}

/// The checksum run prints the same bytes on both engines.
///
/// This is the half a passing suite cannot prove. Each area reduces to an `Int`
/// derived from everything it computed, so one wrong value anywhere changes a
/// checksum — including values no case thought to assert.
#[test]
fn the_harness_checksums_match_across_backends() {
    let path = harness();
    let path = path.to_str().expect("a utf-8 path");
    let vm = kira(&["run", "--backend", "vm", path]);
    let llvm = kira(&["run", "--backend", "llvm", path]);
    assert!(
        vm.status.success() && llvm.status.success(),
        "the checksum run failed: {} {}",
        String::from_utf8_lossy(&vm.stderr),
        String::from_utf8_lossy(&llvm.stderr)
    );
    let (vm, llvm) = (
        String::from_utf8_lossy(&vm.stdout),
        String::from_utf8_lossy(&llvm.stdout),
    );
    assert!(
        vm.starts_with("kik-harness-begin"),
        "unexpected output: {vm}"
    );
    assert!(
        vm.trim_end().ends_with("kik-harness-end"),
        "truncated: {vm}"
    );
    assert_eq!(vm, llvm, "the vm and native backends print different bytes");
}
