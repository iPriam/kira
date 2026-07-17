//! Reload, end to end: a real runner process taking new code without dying.
//!
//! The tier logic has its own unit tests, and they are about manifests. These are
//! about the thing itself: a bundle served over a socket to the real runner
//! binary, then a *second* bundle offered to the same process — and the assertion
//! is on what the app printed, because that is the only evidence that the new
//! code ran and the old process is what ran it.
//!
//! The distinction the whole feature turns on is process identity. A hot patch
//! that quietly relaunched would print exactly the same output; what says
//! otherwise is that the runner reported `reload.completed` on the connection it
//! already had, without the server ever accepting a second one.

use std::io::Read;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use kira_bytecode::{FuncProto, Instruction, Module};
use kira_live::{
    Bundle, ContentHash, LiveEvent, LiveServer, NamedPayload, PayloadKind, ReloadOutcome,
};
use kira_manifest::{BuildProfile, RunnerId};
use kira_runtime_abi::Execution;

/// A module that prints `text` and returns.
fn printing_module(text: &str) -> Module {
    Module {
        main: 0,
        strings: vec![text.to_owned()],
        functions: vec![FuncProto {
            name: "main".to_owned(),
            param_count: 0,
            local_count: 0,
            execution: Execution::Runtime,
            code: vec![
                Instruction::ConstStr(0),
                Instruction::Print,
                Instruction::ReturnVoid,
            ],
        }],
    }
}

/// A VM bundle whose app prints `text`.
fn vm_bundle(text: &str) -> Bundle {
    Bundle::build(
        RunnerId::Desktop,
        BuildProfile::Debug,
        vec![NamedPayload {
            name: "app.kbc".to_owned(),
            kind: PayloadKind::VmBytecode,
            bytes: printing_module(text).to_bytes(),
        }],
        0,
    )
    .expect("a valid bundle")
}

/// A bundle with a bytecode entry and a native library beside it.
///
/// Not a real hybrid bundle — the runner never links it, because these tests
/// stop at the supervisor's decision. It is the shape the decision reads.
fn bundle_with_library(text: &str, library: &[u8]) -> Bundle {
    Bundle::build(
        RunnerId::Desktop,
        BuildProfile::Debug,
        vec![
            NamedPayload {
                name: "app.kbc".to_owned(),
                kind: PayloadKind::VmBytecode,
                bytes: printing_module(text).to_bytes(),
            },
            NamedPayload {
                name: "libapp.dylib".to_owned(),
                kind: PayloadKind::NativeLibrary,
                bytes: library.to_vec(),
            },
        ],
        0,
    )
    .expect("a valid bundle")
}

fn loopback() -> SocketAddr {
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
}

/// A scratch directory that removes itself.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let path = std::env::temp_dir().join(format!("kira-reload-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        TempDir(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A child process that is killed when it goes out of scope.
struct ChildGuard(Option<Child>);

impl ChildGuard {
    fn take(&mut self) -> Child {
        self.0.take().expect("the child is taken exactly once")
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Starts the real runner binary against `address`.
fn spawn_runner(address: SocketAddr, cache: &PathBuf, hotpatch: bool) -> ChildGuard {
    let mut command = Command::new(env!("CARGO_BIN_EXE_kira-desktop-runner"));
    command
        .arg("--server")
        .arg(address.to_string())
        .arg("--cache")
        .arg(cache)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if !hotpatch {
        command.env(kira_live::reload::NO_HOTPATCH_VAR, "1");
    } else {
        // Never inherit the switch from whoever ran the tests: a developer with
        // it exported would silently turn the hot-patch tests into relaunch
        // tests that still pass.
        command.env_remove(kira_live::reload::NO_HOTPATCH_VAR);
    }
    ChildGuard(Some(command.spawn().expect("the runner binary spawns")))
}

/// Reads a finished child's stdout.
fn stdout_of(child: &mut Child) -> String {
    let mut stdout = String::new();
    child
        .stdout
        .take()
        .expect("stdout is piped")
        .read_to_string(&mut stdout)
        .expect("read stdout");
    stdout
}

/// The headline: a real runner takes new code and runs it, in place.
#[test]
fn a_hot_patch_runs_the_new_code_in_the_running_process() {
    let dir = TempDir::new("hotpatch");
    let server = LiveServer::bind(loopback(), vm_bundle("BEFORE")).expect("bind");
    let address = server.local_addr().expect("addr");
    let mut runner = spawn_runner(address, &dir.0, true);

    let mut events = Vec::new();
    let mut session = server
        .accept_session(vm_bundle("BEFORE"), true, &mut |event| events.push(event))
        .expect("the session comes up");

    let outcome = session
        .reload(vm_bundle("AFTER"), false, &mut |event| events.push(event))
        .expect("the reload runs");
    assert_eq!(outcome, ReloadOutcome::HotPatched);

    session.shutdown().expect("shutdown");
    let mut child = runner.take();
    let stdout = stdout_of(&mut child);
    child.wait().expect("the runner exits");

    // Both versions ran, in one process, on one connection. The server never
    // accepted a second runner, so nothing was relaunched.
    assert!(
        stdout.contains("BEFORE"),
        "the original app must have run. stdout: {stdout:?}"
    );
    assert!(
        stdout.contains("AFTER"),
        "the swapped-in code must have run. stdout: {stdout:?}"
    );
}

/// The reload's events are the four the specification names, in order, and each
/// means something different: staged is loaded, applied is committed, completed
/// is proven to have run.
#[test]
fn a_hot_patch_reports_its_milestones_in_order() {
    let dir = TempDir::new("order");
    let server = LiveServer::bind(loopback(), vm_bundle("BEFORE")).expect("bind");
    let address = server.local_addr().expect("addr");
    let _runner = spawn_runner(address, &dir.0, true);

    let mut session = server
        .accept_session(vm_bundle("BEFORE"), true, &mut |_| {})
        .expect("the session comes up");

    let mut events = Vec::new();
    session
        .reload(vm_bundle("AFTER"), false, &mut |event| events.push(event))
        .expect("the reload runs");

    let names: Vec<&str> = events
        .iter()
        .map(LiveEvent::name)
        .filter(|name| name.starts_with("live.reload"))
        .collect();
    assert_eq!(
        names,
        vec![
            "live.reload.notified",
            "live.reload.staged",
            "live.reload.applied",
            "live.reload.completed",
        ]
    );
    let _ = session.shutdown();
}

/// The byte-identity rule, over a real session: the native half moved, so the
/// running process is stale and the supervisor is told to replace it.
#[test]
fn a_native_library_change_needs_a_relaunch() {
    let dir = TempDir::new("native");
    let loaded = bundle_with_library("BEFORE", b"\x7fELF old");
    let server = LiveServer::bind(loopback(), loaded.clone()).expect("bind");
    let address = server.local_addr().expect("addr");
    let _runner = spawn_runner(address, &dir.0, true);

    let mut session = server
        .accept_session(loaded, true, &mut |_| {})
        .expect("the session comes up");

    let mut events = Vec::new();
    let outcome = session
        .reload(
            bundle_with_library("AFTER", b"\x7fELF new"),
            false,
            &mut |event| events.push(event),
        )
        .expect("the reload decides");

    assert!(
        matches!(
            outcome,
            ReloadOutcome::NeedsRelaunch {
                reason: kira_live::RelaunchReason::NativeLibraryChanged { .. }
            }
        ),
        "got {outcome:?}"
    );
    // The user is told why, not just that. A relaunch with no reason is how
    // someone loses their state and never learns what did it.
    let notified = events
        .iter()
        .find(|event| event.name() == "live.reload.notified")
        .expect("the fallback is announced");
    let rendered = notified.to_string();
    assert!(
        rendered.contains("mode=relaunch") && rendered.contains("libapp.dylib"),
        "the relaunch must name what changed: {rendered}"
    );
    let _ = session.shutdown();
}

/// The bytecode moved and the native half did not: the case tier 1 exists for,
/// proven over a real session rather than in the decision's unit tests.
#[test]
fn a_bytecode_only_change_beside_a_native_library_hot_patches() {
    let dir = TempDir::new("beside");
    let loaded = bundle_with_library("BEFORE", b"\x7fELF same");
    let server = LiveServer::bind(loopback(), loaded.clone()).expect("bind");
    let address = server.local_addr().expect("addr");
    let _runner = spawn_runner(address, &dir.0, true);

    let mut session = server
        .accept_session(loaded, true, &mut |_| {})
        .expect("the session comes up");

    let outcome = session
        .reload(
            bundle_with_library("AFTER", b"\x7fELF same"),
            false,
            &mut |_| {},
        )
        .expect("the reload runs");
    assert_eq!(outcome, ReloadOutcome::HotPatched);
    let _ = session.shutdown();
}

/// A save that changed nothing must not disturb a running app at all — no swap,
/// no relaunch, no events.
#[test]
fn an_unchanged_rebuild_does_not_disturb_the_app() {
    let dir = TempDir::new("unchanged");
    let server = LiveServer::bind(loopback(), vm_bundle("SAME")).expect("bind");
    let address = server.local_addr().expect("addr");
    let _runner = spawn_runner(address, &dir.0, true);

    let mut session = server
        .accept_session(vm_bundle("SAME"), true, &mut |_| {})
        .expect("the session comes up");

    let mut events = Vec::new();
    let outcome = session
        .reload(vm_bundle("SAME"), false, &mut |event| events.push(event))
        .expect("the reload decides");

    assert_eq!(outcome, ReloadOutcome::Unchanged);
    assert!(
        events.is_empty(),
        "an unchanged rebuild said something: {events:?}"
    );
    let _ = session.shutdown();
}

/// The kill switch, proven against the real runner binary: with it set, the
/// runner refuses the swap and the session is told to relaunch instead. This is
/// what makes it possible to tell whether a bug belongs to the hot-patch path.
#[test]
fn a_runner_with_the_kill_switch_set_refuses_to_hot_patch() {
    let dir = TempDir::new("killswitch");
    let server = LiveServer::bind(loopback(), vm_bundle("BEFORE")).expect("bind");
    let address = server.local_addr().expect("addr");
    let _runner = spawn_runner(address, &dir.0, false);

    let mut session = server
        .accept_session(vm_bundle("BEFORE"), true, &mut |_| {})
        .expect("the session comes up");

    // The supervisor still attempts tier 1 — the switch is the runner's, and the
    // runner is the one that says no.
    let mut events = Vec::new();
    let outcome = session
        .reload(vm_bundle("AFTER"), false, &mut |event| events.push(event))
        .expect("the reload runs");

    assert!(
        matches!(
            outcome,
            ReloadOutcome::NeedsRelaunch {
                reason: kira_live::RelaunchReason::RunnerRefused { .. }
            }
        ),
        "got {outcome:?}"
    );
    assert!(
        events
            .iter()
            .any(|event| event.name() == "live.reload.restart_required"),
        "the runner's refusal must be reported: {events:?}"
    );
    let _ = session.shutdown();
}

/// The supervisor's own kill switch, which does not even attempt tier 1.
#[test]
fn a_supervisor_with_hotpatch_disabled_never_attempts_a_swap() {
    let dir = TempDir::new("supervisor-off");
    let server = LiveServer::bind(loopback(), vm_bundle("BEFORE")).expect("bind");
    let address = server.local_addr().expect("addr");
    let _runner = spawn_runner(address, &dir.0, true);

    let mut session = server
        .accept_session(vm_bundle("BEFORE"), true, &mut |_| {})
        .expect("the session comes up");

    let mut events = Vec::new();
    let outcome = session
        .reload(vm_bundle("AFTER"), true, &mut |event| events.push(event))
        .expect("the reload decides");

    assert!(
        matches!(
            outcome,
            ReloadOutcome::NeedsRelaunch {
                reason: kira_live::RelaunchReason::DisabledByEnv
            }
        ),
        "got {outcome:?}"
    );
    // Nothing was staged or applied: the swap was never attempted, so the runner
    // was never asked.
    assert!(
        !events
            .iter()
            .any(|event| event.name() == "live.reload.staged"),
        "a disabled hot patch still staged something: {events:?}"
    );
    let _ = session.shutdown();
}

/// A bundle's payloads are hashed, and the hash is what the decision reads. If
/// two different programs hashed the same, a hot patch would swap code the
/// process cannot take — so this pins that the two test bundles really do differ.
#[test]
fn the_test_bundles_actually_differ() {
    let before = vm_bundle("BEFORE");
    let after = vm_bundle("AFTER");
    assert_ne!(
        before.manifest().payloads[0].hash,
        after.manifest().payloads[0].hash
    );
    assert_eq!(
        before.manifest().payloads[0].hash,
        ContentHash::of(printing_module("BEFORE").to_bytes().as_slice())
    );
}
