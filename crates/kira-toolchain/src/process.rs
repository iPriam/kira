//! How a toolchain binary leaves.
//!
//! `kira` and the language server both link LLVM, and LLVM's C++ globals
//! register destructors with `atexit`. Running those is what `exit(3)` does,
//! and here it is both pointless and unsafe.
//!
//! Pointless, because the process is ending: every byte those destructors free
//! is about to be reclaimed by the kernel anyway, and LLVM's static state is
//! hundreds of megabytes of it.
//!
//! Unsafe, because it has aborted the process. `llvm::TensorSpec::~TensorSpec`
//! running under `__run_exit_handlers` has killed `kira` with `double free or
//! corruption (!prev)` *after* the compiler finished its work and printed its
//! diagnostics — so a build that had already succeeded reported a signal, and a
//! sixty-megabyte core landed in the journal for it. Intermittently, because
//! whether a corrupted free trips the allocator's checks depends on the heap
//! layout it happens to find, which is exactly what makes it unacceptable to
//! leave in: it turns every invocation into a coin flip that CI reads as a
//! crash.
//!
//! So the process leaves without running them. The one thing `exit(3)` does
//! that is still wanted is flushing the standard streams, and that is done
//! here first — explicitly, because `_exit` will not do it.
//!
//! This is not a workaround for a bug elsewhere that ought to be fixed. A
//! compiler driver has no reason to tear down its own address space on the way
//! out, and the ones that do it anyway pay for it in shutdown time and in
//! exactly this class of crash.

use std::io::Write;

/// Ends the process with `code`, without running C++ static destructors.
///
/// Spelled like [`std::process::exit`] and used in its place throughout the
/// toolchain's binaries, so a call site reads the same and the difference is
/// the import.
///
/// Nothing is lost by not running the handlers: no Kira code registers one,
/// the build lock is a `flock` the kernel drops when the process does, and
/// `Drop` never ran on this path to begin with — `std::process::exit` does not
/// unwind either.
pub fn exit(code: i32) -> ! {
    // Before the streams stop existing. A `print!` with no trailing newline is
    // still sitting in the buffer at this point, and `_exit` would drop it.
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    // SAFETY: `_exit` takes an exit status and does not return. It is async
    // signal safe and touches nothing this process still owns — which is the
    // whole reason it is the one called here.
    unsafe { libc::_exit(code) }
}
