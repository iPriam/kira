//! An app, not a program: a runner hosting something that does not end.
//!
//! Every other live test runs a program that prints and returns, and a runner
//! that only ever hosts those can get away with reporting `entrypoint started`
//! after the entrypoint *finished*. A real Kira app never finishes — it opens a
//! window and its run loop owns the thread until the window closes — so a runner
//! built that way reports nothing, the server waits out its read timeout, and the
//! session fails with a socket error thirty seconds after the app came up fine.
//!
//! So the app here loops forever, and the assertions are the things that were
//! impossible before: the session becomes ready while the app is still running,
//! and a reload offered to a running app is refused in words rather than hung
//! on. The other half is the app that *does* end — which the runner now has to
//! report, because "ready" no longer means "finished" and an unwatched session
//! has nothing else to wait for.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use kira_bytecode::{FrameRelease, FuncProto, Instruction, Module};
use kira_live::{
    Bundle, LiveEvent, LiveServer, NamedPayload, PayloadKind, RelaunchReason, ReloadOutcome,
    SessionPhase,
};
use kira_manifest::{BuildProfile, RunnerId};
use kira_runtime_abi::Execution;

/// A module that prints `text` and then never returns.
///
/// The `Jump` targets itself, which is what an app's run loop looks like from
/// outside: the entrypoint is running, it is not going to stop, and the runner
/// has to be able to say so.
fn looping_module(text: &str) -> Module {
    Module {
        exports: Default::default(),
        foreign_imports: Vec::new(),
        foreign_aggregates: Default::default(),
        foreign_callbacks: Vec::new(),
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
                Instruction::Jump(2),
                Instruction::ReturnVoid,
            ],
            releases: FrameRelease::EveryLocal,
        }],
    }
}

/// A module that prints `text` and returns, like a program rather than an app.
fn printing_module(text: &str) -> Module {
    let mut module = looping_module(text);
    module.functions[0].code = vec![
        Instruction::ConstStr(0),
        Instruction::Print,
        Instruction::ReturnVoid,
    ];
    module
}

/// A module that traps on its first divide.
fn trapping_module() -> Module {
    let mut module = looping_module("about to trap");
    module.functions[0].code = vec![
        Instruction::ConstInt(1),
        Instruction::ConstInt(0),
        Instruction::DivInt,
        Instruction::Pop,
        Instruction::ReturnVoid,
    ];
    module
}

/// A VM bundle around one module.
fn bundle_of(module: Module) -> Bundle {
    Bundle::build(
        RunnerId::Desktop,
        BuildProfile::Debug,
        vec![NamedPayload {
            name: "app.kbc".to_owned(),
            kind: PayloadKind::VmBytecode,
            bytes: module.to_bytes(),
        }],
        0,
    )
    .expect("a valid bundle")
}

/// A VM bundle whose app prints `text` and then runs forever.
fn app_bundle(text: &str) -> Bundle {
    bundle_of(looping_module(text))
}

fn loopback() -> SocketAddr {
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
}

/// A scratch directory that removes itself.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let path = std::env::temp_dir().join(format!("kira-live-app-{}-{tag}", std::process::id()));
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
///
/// It has to be: this app never exits on its own, so a test that failed without
/// this would leave a process spinning a core until the machine was rebooted.
struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The runner binary, as cargo built it beside this test.
fn runner_binary() -> PathBuf {
    let mut path = std::env::current_exe().expect("the test binary has a path");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.join(format!(
        "kira-desktop-runner{}",
        std::env::consts::EXE_SUFFIX
    ))
}

/// Starts the real runner binary against `address`.
fn spawn_runner(address: SocketAddr, cache: &PathBuf) -> ChildGuard {
    let child = Command::new(runner_binary())
        .arg("--server")
        .arg(address.to_string())
        .arg("--cache")
        .arg(cache)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the runner binary starts");
    ChildGuard(child)
}

/// The regression this whole split exists for: an app that is still running is a
/// session that is ready, not a session that timed out waiting for it to finish.
#[test]
fn a_running_app_reaches_a_ready_session() {
    let bundle = app_bundle("app up");
    let cache = TempDir::new("ready");
    let server = LiveServer::bind(loopback(), bundle.clone()).expect("bind");
    let address = server.local_addr().expect("addr");

    let _runner = spawn_runner(address, &cache.0);
    let session = server
        .accept_session(bundle, true, &mut |_| {})
        .expect("the session is ready while the app is still running");

    assert!(
        session
            .progress()
            .has_reached(SessionPhase::EntrypointStarted),
        "the runner must report the entrypoint of an app that has not finished"
    );
}

/// A swap under a running app would pull the module out from under a live call
/// stack. The runner refuses in its own words, and the session relaunches — which
/// is the whole reload story for an app until a swap point exists inside one.
#[test]
fn a_reload_under_a_running_app_relaunches_rather_than_swapping() {
    let bundle = app_bundle("first");
    let cache = TempDir::new("reload");
    let server = LiveServer::bind(loopback(), bundle.clone()).expect("bind");
    let address = server.local_addr().expect("addr");

    let _runner = spawn_runner(address, &cache.0);
    let mut session = server
        .accept_session(bundle, true, &mut |_| {})
        .expect("the session is ready");

    let outcome = session
        .reload(app_bundle("second"), false, &mut |_| {})
        .expect("offering a rebuilt bundle is answered, not hung on");

    match outcome {
        ReloadOutcome::NeedsRelaunch {
            reason: RelaunchReason::RunnerRefused { reason },
        } => assert!(
            reason.contains("still running"),
            "the refusal must name the running entrypoint, got `{reason}`"
        ),
        other => panic!("a running app cannot be hot patched, got {other:?}"),
    }
}

/// A program that returns is an app that ended, and the runner says so on the
/// connection it still has. Without it an unwatched session would have nothing
/// to wait for and would shut the app down the instant it started.
#[test]
fn an_app_that_finishes_reports_its_exit() {
    let bundle = bundle_of(printing_module("done"));
    let cache = TempDir::new("finishes");
    let server = LiveServer::bind(loopback(), bundle.clone()).expect("bind");
    let address = server.local_addr().expect("addr");

    let _runner = spawn_runner(address, &cache.0);
    let mut session = server
        .accept_session(bundle, true, &mut |_| {})
        .expect("the session is ready");

    let mut events = Vec::new();
    session
        .wait_for_app_exit(&mut |event| events.push(event))
        .expect("the app's exit arrives");

    assert!(session.app_exited(), "the session must record the exit");
    assert!(
        events.contains(&LiveEvent::AppExited { reason: None }),
        "an app that finished reports no reason, got {events:?}"
    );
}

/// An app that trapped stopped for a reason, and the reason is the app's own
/// words rather than a session's guess about why it went quiet.
#[test]
fn an_app_that_traps_reports_why_it_stopped() {
    let bundle = bundle_of(trapping_module());
    let cache = TempDir::new("traps");
    let server = LiveServer::bind(loopback(), bundle.clone()).expect("bind");
    let address = server.local_addr().expect("addr");

    let _runner = spawn_runner(address, &cache.0);
    let mut session = server
        .accept_session(bundle, true, &mut |_| {})
        .expect("a trapping app still starts");

    let mut events = Vec::new();
    session
        .wait_for_app_exit(&mut |event| events.push(event))
        .expect("the app's exit arrives");

    let reported = events.iter().find_map(|event| match event {
        LiveEvent::AppExited { reason } => reason.clone(),
        _ => None,
    });
    let reason = reported.expect("a trapping app reports a reason");
    assert!(
        reason.contains("vm"),
        "the reason must be the runner's own account of the trap, got `{reason}`"
    );
}
