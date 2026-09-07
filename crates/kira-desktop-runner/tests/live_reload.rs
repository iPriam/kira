//! Reload, end to end: a real runner process taking new code without dying.
//!
//! The tier logic has its own unit tests, and these exercise manifest decisions
//! through a real runner process.
//!
//! Changed bytecode requests a relaunch until the bundle carries live-value
//! compatibility evidence.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use kira_bytecode::{FrameRelease, FuncProto, Instruction, Module};
use kira_live::{
    Bundle, ContentHash, LiveEvent, LiveServer, NamedPayload, PayloadKind, ReloadOutcome,
};
use kira_manifest::{BuildProfile, RunnerId};
use kira_runtime_abi::Execution;

/// A module that prints `text` and returns.
fn printing_module(text: &str) -> Module {
    Module {
        exports: Default::default(),
        foreign_imports: Vec::new(),
        foreign_aggregates: Default::default(),
        foreign_callbacks: Vec::new(),
        constants: Vec::new(),
        types: Vec::new(),
        main: Some(0),
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
            releases: FrameRelease::EveryLocal,
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
        // it exported would silently change the reload decision under test.
        command.env_remove(kira_live::reload::NO_HOTPATCH_VAR);
    }
    ChildGuard(Some(command.spawn().expect("the runner binary spawns")))
}

/// A changed bytecode module requests a relaunch.
#[test]
fn a_changed_bytecode_requests_a_relaunch() {
    let dir = TempDir::new("hotpatch");
    let server = LiveServer::bind(loopback(), vm_bundle("BEFORE")).expect("bind");
    let address = server.local_addr().expect("addr");
    let _runner = spawn_runner(address, &dir.0, true);

    let mut events = Vec::new();
    let mut session = server
        .accept_session(vm_bundle("BEFORE"), true, &mut |event| events.push(event))
        .expect("the session comes up");

    let outcome = session
        .reload(vm_bundle("AFTER"), false, &mut |event| events.push(event))
        .expect("the reload runs");
    assert_eq!(
        outcome,
        ReloadOutcome::NeedsRelaunch {
            reason: kira_live::RelaunchReason::BytecodeChanged {
                payload: "app.kbc".to_owned(),
            },
        }
    );
    session.shutdown().expect("shutdown");
}

/// A relaunch decision does not stage or apply a replacement in the current
/// runner process.
#[test]
fn a_changed_bytecode_reports_only_the_relaunch_decision() {
    let dir = TempDir::new("order");
    let server = LiveServer::bind(loopback(), vm_bundle("BEFORE")).expect("bind");
    let address = server.local_addr().expect("addr");
    let _runner = spawn_runner(address, &dir.0, true);

    let mut session = server
        .accept_session(vm_bundle("BEFORE"), true, &mut |_| {})
        .expect("the session comes up");

    let mut events = Vec::new();
    let outcome = session
        .reload(vm_bundle("AFTER"), false, &mut |event| events.push(event))
        .expect("the reload runs");

    assert!(matches!(
        outcome,
        ReloadOutcome::NeedsRelaunch {
            reason: kira_live::RelaunchReason::BytecodeChanged { .. }
        }
    ));

    let names: Vec<&str> = events
        .iter()
        .map(LiveEvent::name)
        .filter(|name| name.starts_with("live.reload"))
        .collect();
    assert_eq!(names, vec!["live.reload.notified"]);
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
            bundle_with_library("BEFORE", b"\x7fELF new"),
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

/// Changed bytecode requests a relaunch even when another native payload stays
/// byte-identical.
#[test]
fn a_bytecode_only_change_beside_a_native_library_needs_a_relaunch() {
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
    assert_eq!(
        outcome,
        ReloadOutcome::NeedsRelaunch {
            reason: kira_live::RelaunchReason::BytecodeChanged {
                payload: "app.kbc".to_owned(),
            },
        }
    );
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

/// Changed bytecode is rejected before a runner-side hot-patch kill switch is
/// consulted.
#[test]
fn a_changed_bytecode_does_not_reach_the_runner_kill_switch() {
    let dir = TempDir::new("killswitch");
    let server = LiveServer::bind(loopback(), vm_bundle("BEFORE")).expect("bind");
    let address = server.local_addr().expect("addr");
    let _runner = spawn_runner(address, &dir.0, false);

    let mut session = server
        .accept_session(vm_bundle("BEFORE"), true, &mut |_| {})
        .expect("the session comes up");

    let mut events = Vec::new();
    let outcome = session
        .reload(vm_bundle("AFTER"), false, &mut |event| events.push(event))
        .expect("the reload runs");

    assert!(
        matches!(
            outcome,
            ReloadOutcome::NeedsRelaunch {
                reason: kira_live::RelaunchReason::BytecodeChanged { .. }
            }
        ),
        "got {outcome:?}"
    );
    assert_eq!(events.len(), 1);
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
/// two different programs hashed the same, a reload could accept stale code, so
/// this pins that the two test bundles really do differ.
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
