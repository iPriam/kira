//! The compiler `kira` grants a program it runs.
//!
//! `kira` already links the frontend, so it is the one host that can answer
//! `kcCheckPackages`. It grants that once, at startup, by installing a checker
//! into [`kira_runtime_abi::compiler`]; every host in this process — the VM-only
//! host, and the runtime half of a hybrid session — answers through it from
//! then on.
//!
//! Installing rather than wrapping is what makes the hybrid case work. A hybrid
//! run has two engines in one process and the runtime half's host is built
//! inside `kira-hybrid-runtime`, which is layer 4 and may not know a compiler
//! exists. A process-wide slot is the only place both can reach.
//!
//! An embedder that never calls [`grant`] keeps the refusing default, which is
//! the point: a VM with no compiler says so by name.

use std::sync::Mutex;

use kira_check::CheckSession;
use kira_runtime_abi::{CheckDiagnostic, CheckRequest, PackageChecker};

/// The checker `kira` installs: one session, reused across every call.
///
/// The session is what makes a suite of many checks affordable — it reads the
/// bundled packages once — and the mutex is what makes it usable from a
/// `PackageChecker`, which the capability requires to be `Send`.
struct KiracChecker {
    session: Mutex<CheckSession>,
}

impl PackageChecker for KiracChecker {
    fn check(&mut self, request: &CheckRequest) -> Vec<CheckDiagnostic> {
        match self.session.lock() {
            Ok(mut session) => session.check(request),
            // A poisoned lock means a previous check panicked. Rebuilding the
            // session is the honest recovery: it holds only cached reads, so a
            // fresh one answers exactly what a poisoned one would have.
            Err(poisoned) => {
                let mut session = poisoned.into_inner();
                *session = CheckSession::new();
                session.check(request)
            }
        }
    }
}

/// Grants every host in this process the compiler `kira` links.
pub fn grant() {
    kira_runtime_abi::compiler::install(Box::new(KiracChecker {
        session: Mutex::new(CheckSession::new()),
    }));
}
