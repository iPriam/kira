//! The standalone hybrid program: the executable `kira build --backend hybrid`
//! stages beside a bundle so a built program is something the operating system
//! starts, not something only the compiler can host.
//!
//! A hybrid build's machine code lives in a shared library and its bytecode lives
//! in a `.kbc`, tied together by a `.khm` manifest; running all three needs both
//! of Kira's engines in one process, which is what [`kira_hybrid_runtime::Session`]
//! is. This binary is the thinnest possible shell around one: resolve which
//! manifest to run (see [`kira_hybrid_launcher`]), load it, run it to completion,
//! and report any failure the way every Kira runner does — one line on stderr,
//! prefixed with this runner's name, and a non-zero exit.
//!
//! Deliberately absent: flags. The first argument is either a `.khm` path or the
//! program's own first argument, so a program's flag grammar is never competing
//! with the launcher's.

use std::process::ExitCode;

use kira_hybrid_launcher::{InvocationError, resolve};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("kira-hybrid-launcher: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), RunError> {
    let arguments: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let executable = std::env::current_exe().map_err(RunError::LocateSelf)?;
    let invocation = resolve(&arguments, &executable)?;

    let mut child = std::process::Command::new(&executable);
    child
        .arg(&invocation.manifest)
        .args(&invocation.program_arguments);
    if kira_hybrid_launcher::configure_sanitizer_child(&mut child, &invocation.manifest)? {
        let status = child.status().map_err(RunError::SpawnSanitized)?;
        if status.success() {
            return Ok(());
        }
        return Err(RunError::SanitizedChildFailed(status.code()));
    }

    let session = kira_hybrid_runtime::Session::load(&invocation.manifest)?;
    // SAFETY: as above — this thread owns the process environment for the
    // whole run.
    unsafe {
        kira_runtime_abi::env::with_arguments(&invocation.program_arguments, || session.run())?;
    }
    Ok(())
}

/// Everything that stops a standalone run before the program returns.
#[derive(Debug, thiserror::Error)]
enum RunError {
    /// The running image could not be located, so there is no default manifest.
    #[error("cannot locate this executable: {0}")]
    LocateSelf(std::io::Error),
    /// The invocation did not name a usable manifest.
    #[error(transparent)]
    Invocation(#[from] InvocationError),
    /// Loading or running the bundle failed.
    #[error(transparent)]
    Session(#[from] kira_hybrid_runtime::HybridError),
    /// The sanitizer runtime could not be installed before the native half.
    #[error(transparent)]
    Sanitizer(#[from] kira_hybrid_launcher::SanitizerLaunchError),
    /// The sanitized launcher child could not start.
    #[error("cannot start the sanitized hybrid child: {0}")]
    SpawnSanitized(std::io::Error),
    /// The sanitized launcher child failed.
    #[error("the sanitized hybrid child failed with exit code {0:?}")]
    SanitizedChildFailed(Option<i32>),
}
