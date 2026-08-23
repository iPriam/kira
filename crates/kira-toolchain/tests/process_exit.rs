//! `process::exit` leaves without running the C++ static destructors, and
//! still flushes what was printed.
//!
//! Both halves need a real process to observe, because the thing under test is
//! how one ends: a handler that did run would run in the child, and a buffer
//! that was not flushed would be lost with it. So the test re-executes its own
//! binary, has the child register an `atexit` handler and leave a byte in the
//! stdout buffer, and reads the child's output back.
//!
//! The handler writes with `write(2)` rather than `println!`: it runs at a
//! point where Rust's stdio has no promises left, and a marker that failed to
//! appear for that reason would look exactly like the handler not running,
//! which is the thing being asserted.

#![cfg(unix)]

use std::process::Command;

/// The variable that tells a re-executed child it is the subject.
const CHILD: &str = "KIRA_TOOLCHAIN_EXIT_CHILD";

/// Printed by the `atexit` handler, and by nothing else.
const HANDLER_MARKER: &str = "ATEXIT-HANDLER-RAN";

/// Printed with no trailing newline, so it is still sitting in the buffer when
/// the process leaves. `_exit` does not flush, so this only arrives if
/// `process::exit` flushed it on the way out.
const BUFFERED_MARKER: &str = "BUFFERED-WITHOUT-A-NEWLINE";

/// The status the child leaves with — arbitrary, and not 0 or 1, so it cannot
/// be confused with the harness succeeding or failing on its own.
const CHILD_STATUS: i32 = 7;

extern "C" fn handler() {
    let message = b"ATEXIT-HANDLER-RAN\n";
    // SAFETY: a write of `message`'s own bytes to the standard output
    // descriptor, which is open for the life of the process.
    unsafe {
        libc::write(1, message.as_ptr().cast(), message.len());
    }
}

#[test]
fn exiting_runs_no_handlers_and_still_flushes() {
    if std::env::var_os(CHILD).is_some() {
        // SAFETY: `handler` is an `extern "C"` function with the signature
        // `atexit` takes, and it lives for the whole program.
        unsafe {
            libc::atexit(handler);
        }
        print!("{BUFFERED_MARKER}");
        kira_toolchain::process::exit(CHILD_STATUS);
    }

    let executable = std::env::current_exe().expect("the test binary's own path");
    let output = Command::new(executable)
        .args([
            "--exact",
            "exiting_runs_no_handlers_and_still_flushes",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .output()
        .expect("re-execute this test binary as the child");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert_eq!(
        output.status.code(),
        Some(CHILD_STATUS),
        "the status must survive the shortcut; stdout was: {stdout}"
    );
    assert!(
        stdout.contains(BUFFERED_MARKER),
        "output buffered without a newline must still be flushed; stdout was: {stdout}"
    );
    // The claim the crash was about: `llvm::TensorSpec::~TensorSpec` is
    // registered exactly the way this handler is, and running it at exit is
    // what aborted `kira` after a build had already succeeded.
    assert!(
        !stdout.contains(HANDLER_MARKER),
        "no `atexit` handler may run on the way out; stdout was: {stdout}"
    );
}
