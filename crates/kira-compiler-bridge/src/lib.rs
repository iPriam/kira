//! The native half of Kira's compiler capability: `kira_rt_compiler_*`.
//!
//! Layer 8 of the Kira package graph. Native-only, and outside the portable VM
//! core for a stronger reason than the rest of the native runtime: this one
//! *contains a compiler*.
//!
//! # Why this is a separate archive
//!
//! The VM reaches a compiler through its host, and a native program has no
//! host — its `main` is the program. So the only way native code can check a
//! package is for the frontend to be linked into it, and the only place that
//! can happen is above `kira-check`, which is layer 7. `kira-native-bridge` is
//! layer 4 and may not depend on it, so this crate sits on top of both and
//! produces a runtime archive that is a **superset** of the base one: every
//! `kira_rt_*` symbol the base archive defines is here too, because the base
//! crate is linked into this staticlib.
//!
//! A native build therefore links one archive or the other, never both — two
//! Rust static libraries in one link line duplicate the standard library and
//! fail. `kira` picks by asking the program: [`IrProgram::uses_compiler`] is
//! true and it links this one, false and it links the small one and no Kira
//! program pays for a compiler it never calls.
//!
//! [`IrProgram::uses_compiler`]: https://docs.rs/kira-ir
//!
//! # Refusal
//!
//! A host that does not provide the capability refuses *by name*, and here that
//! name is the linker's: an embedder that links only the base archive and runs
//! a program calling `kcCheckPackages` fails to link on the undefined symbol
//! `kira_rt_compiler_check_packages`. It never silently answers "no
//! diagnostics", which a caller would read as "it compiled".
//!
//! # Ownership
//!
//! Affine, like the rest of this runtime: the helper consumes the array it is
//! given and returns one the caller owns.

use std::cell::RefCell;

use kira_check::CheckSession;
use kira_native_bridge::array::KArray;
use kira_native_bridge::values::{string_array, take_string_array};
use kira_runtime_abi::{CheckDiagnostic, CheckRequest, ToolRequest, ToolVerb};

thread_local! {
    /// The compiler this thread checks with.
    ///
    /// One session rather than one per call, for the reason the session exists:
    /// a suite of a thousand checks would otherwise walk and read the bundled
    /// packages a thousand times. Thread-local rather than global because a
    /// session is `&mut` to use and a lock on a compiler is a deadlock waiting
    /// for a program that checks a package from two threads.
    static SESSION: RefCell<CheckSession> = RefCell::new(CheckSession::new());
}

/// Checks a package set, answering with its diagnostics.
///
/// Both arrays are `[String]`: the request in [`CheckRequest`]'s layout, the
/// answer in [`CheckDiagnostic`]'s. The engine seam carries no other shape, and
/// the layout is spelled once in `kira-runtime-abi` so this and the VM read the
/// same bytes the same way.
///
/// A request that cannot be read answers with an empty array rather than
/// trapping: this is the native half of a capability whose whole contract is
/// that asking is always allowed. The VM reports the same condition as a trap
/// because it can — it decodes before the call, where a trap is still possible.
///
/// # Safety
/// `request` must be null or a live array of string handles built with `esize`,
/// and is freed here; `esize` must be the ABI size the backend gives a
/// string-handle element.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_compiler_check_packages(request: KArray, esize: i64) -> KArray {
    // SAFETY: forwarded contract; this consumes the array and its handles.
    let fields = unsafe { take_string_array(request, esize) };
    let diagnostics = match CheckRequest::decode(&fields) {
        Ok(request) => SESSION.with_borrow_mut(|session| session.check(&request)),
        Err(_) => Vec::new(),
    };
    // SAFETY: forwarded `esize` contract.
    unsafe { string_array(&CheckDiagnostic::encode(&diagnostics), esize) }
}

/// Checks the package at a path, answering with its diagnostics.
///
/// # Safety
/// `request` must be null or a live array of string handles built with `esize`,
/// and is freed here; `esize` must be the ABI size the backend gives a
/// string-handle element.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_compiler_check_path(request: KArray, esize: i64) -> KArray {
    // SAFETY: forwarded contract; this consumes the array and its handles.
    let fields = unsafe { take_string_array(request, esize) };
    // SAFETY: forwarded `esize` contract.
    unsafe { string_array(&toolchain(ToolVerb::Check, &fields), esize) }
}

/// Builds the package at a path, answering with its diagnostics.
///
/// # Safety
/// The same contract [`kira_rt_compiler_check_path`] carries.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_compiler_build_path(request: KArray, esize: i64) -> KArray {
    // SAFETY: forwarded contract; this consumes the array and its handles.
    let fields = unsafe { take_string_array(request, esize) };
    // SAFETY: forwarded `esize` contract.
    unsafe { string_array(&toolchain(ToolVerb::Build, &fields), esize) }
}

/// Builds the package at a path and runs it, answering with its exit code.
///
/// # Safety
/// The same contract [`kira_rt_compiler_check_path`] carries.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_compiler_run_path(request: KArray, esize: i64) -> KArray {
    // SAFETY: forwarded contract; this consumes the array and its handles.
    let fields = unsafe { take_string_array(request, esize) };
    // SAFETY: forwarded `esize` contract.
    unsafe { string_array(&toolchain(ToolVerb::Run, &fields), esize) }
}

/// Performs one toolchain verb against whatever this process installed.
///
/// Unlike checking a package set held in memory, this crate cannot answer on
/// its own: it contains a frontend, and building a project on a disk needs a
/// backend, a linker, and somewhere to put what they produce. So the native
/// half asks the process-wide slot, which the program that owns the build fills
/// — `kira` does it at startup, and a hybrid run's native half reaches the one
/// its runtime half is using.
///
/// A process that installed none traps, and does not answer. A refused request
/// must never come back as an empty diagnostic list, because no diagnostics is
/// exactly what "it compiled" looks like, and a standalone native binary — one
/// that was compiled ahead of time and ships without a build system — is
/// precisely where that would be read as a clean check of a package it never
/// looked at. Trapping is the native mirror of the VM raising `VmError`: the
/// same refusal, said the only way native code can say it.
fn toolchain(verb: ToolVerb, fields: &[String]) -> Vec<String> {
    let request = match ToolRequest::decode(fields) {
        Ok(request) => request,
        Err(error) => trap_toolchain(&error.to_string()),
    };
    match kira_runtime_abi::toolchain::perform(verb, &request) {
        Ok(answer) => answer.encode(),
        Err(error) => trap_toolchain(&error.to_string()),
    }
}

/// Ends the process on a toolchain the program cannot reach.
///
/// The native counterpart of a VM trap: it names what went wrong and stops,
/// rather than letting a program continue on an answer it did not get. A
/// standalone native build has no build system to install one, so a program
/// that calls a toolchain verb from a shipped binary lands here.
fn trap_toolchain(reason: &str) -> ! {
    eprintln!("kira: runtime trap: toolchain operation failed: {reason}");
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    use super::*;
    use kira_runtime_abi::{CheckFile, CheckPackage, CheckSeverity};

    /// The stride generated code gives a handle element on this target.
    const HANDLE: i64 = size_of::<*mut u8>() as i64;

    fn request(text: &str) -> CheckRequest {
        CheckRequest {
            root: "App".to_owned(),
            packages: vec![CheckPackage {
                manifest: "Package App {\n    let kind = .App\n}\n".to_owned(),
                files: vec![CheckFile {
                    path: "app/main.kira".to_owned(),
                    text: text.to_owned(),
                }],
            }],
        }
    }

    /// Drives the helper exactly as generated code does, and reads its answer.
    fn check(request: &CheckRequest) -> Vec<CheckDiagnostic> {
        // SAFETY: `HANDLE` is this target's handle stride; the argument array is
        // consumed by the helper and the answer is consumed by the reader.
        let answer = unsafe {
            let argument = string_array(&request.encode(), HANDLE);
            kira_rt_compiler_check_packages(argument, HANDLE)
        };
        // SAFETY: the answer was built with the same stride.
        let fields = unsafe { take_string_array(answer, HANDLE) };
        CheckDiagnostic::decode(&fields).expect("the helper writes its own layout")
    }

    #[test]
    fn a_package_that_compiles_answers_with_no_diagnostics() {
        let diagnostics = check(&request("@Main function main() { return }"));
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    #[test]
    fn a_package_that_does_not_compile_answers_with_its_code_and_file() {
        let diagnostics = check(&request("@Main function main() { print(missing) return }"));
        let reported: Vec<(&str, &str)> = diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == CheckSeverity::Error)
            .map(|diagnostic| (diagnostic.code.as_str(), diagnostic.file.as_str()))
            .collect();
        assert_eq!(reported, vec![("KSEM060", "app/main.kira")]);
    }

    /// A malformed request answers rather than trapping: the native half has no
    /// trap to raise that would not end the caller's process.
    #[test]
    fn an_unreadable_request_answers_with_nothing() {
        // SAFETY: an empty array is a live array; both are freed by the calls.
        let answer = unsafe {
            let argument = string_array(&[], HANDLE);
            kira_rt_compiler_check_packages(argument, HANDLE)
        };
        // SAFETY: the answer was built with the same stride.
        let fields = unsafe { take_string_array(answer, HANDLE) };
        assert!(fields.is_empty());
    }
}
