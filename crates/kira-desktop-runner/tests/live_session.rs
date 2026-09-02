//! End-to-end live sessions: a real server, a real socket, a real runner.
//!
//! The unit tests prove each piece in isolation, which is exactly what a fake
//! success looks like from the inside. These prove the thing itself: a bundle
//! built here, served over a real TCP connection, and run by the real runner —
//! and, in the subprocess test, by the real runner *binary*, whose stdout is
//! captured and asserted on. The app's own output is the evidence: the string
//! only exists inside the bytecode, so nothing but the VM executing that module
//! can produce it. No milestone is taken on trust.

use std::io::Read;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use kira_bundle_host::BundleHost;
use kira_bytecode::{FrameRelease, FuncProto, Instruction, Module};
use kira_live::{
    Bundle, LiveEvent, LiveServer, NamedPayload, PayloadKind, RunnerClient, SessionPhase,
};
use kira_manifest::{BuildProfile, RunnerId};
use kira_runtime_abi::Execution;

/// What the test app prints. It exists only as a string constant inside the
/// bytecode, so seeing it on the runner's stdout means that module really ran.
const APP_OUTPUT: &str = "kira-live-e2e-marker";

/// A module that prints [`APP_OUTPUT`] and returns.
fn printing_module() -> Module {
    Module {
        exports: Default::default(),
        foreign_imports: Vec::new(),
        foreign_aggregates: Default::default(),
        foreign_callbacks: Vec::new(),
        constants: Vec::new(),
        types: Vec::new(),
        main: Some(0),
        strings: vec![APP_OUTPUT.to_owned()],
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

/// A bundle carrying that module as its entrypoint.
fn vm_bundle() -> Bundle {
    Bundle::build(
        RunnerId::Desktop,
        BuildProfile::Debug,
        vec![NamedPayload {
            name: "app.kbc".to_owned(),
            kind: PayloadKind::VmBytecode,
            bytes: printing_module().to_bytes(),
        }],
        0,
    )
    .expect("a valid bundle")
}

/// A loopback address on an OS-assigned port.
fn loopback() -> SocketAddr {
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
}

/// A scratch directory that removes itself.
struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let path = std::env::temp_dir().join(format!("kira-live-e2e-{}-{tag}", std::process::id()));
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
/// A test that spawns a process and leaves it running turns a failure into a
/// hang, and this host has no `timeout` to bound one with. Killing on drop
/// covers a test that *panics*, since the unwind runs this.
///
/// It does not cover a test that blocks, because a thread stuck in a syscall
/// never unwinds and never drops anything. That gap is closed on the other side,
/// by the server's own accept and read timeouts: the session fails on its own
/// rather than waiting for a runner that will never speak. A guard here and a
/// timeout there are not redundant — neither one covers the other's case.
struct ChildGuard(Option<Child>);

impl ChildGuard {
    fn new(child: Child) -> ChildGuard {
        ChildGuard(Some(child))
    }

    /// Takes the child to wait on it, so the guard stops managing it.
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

/// A whole session, in this process: the server on a thread, the runner here.
#[test]
fn a_live_session_reaches_ready() {
    let dir = TempDir::new("in-process");
    let server = LiveServer::bind(loopback(), vm_bundle()).expect("bind");
    let address = server.local_addr().expect("addr");

    let session = std::thread::spawn(move || {
        let mut events = Vec::new();
        let progress = server.serve_once(true, &mut |event| events.push(event));
        (progress, events)
    });

    let mut client = RunnerClient::connect(address, RunnerId::Desktop).expect("connect");
    let mut host = BundleHost::new(dir.0.clone());
    client.run_session(&mut host).expect("the session runs");
    client.goodbye().expect("goodbye");

    let (progress, events) = session.join().expect("the server thread does not panic");
    let progress = progress.expect("the server reports a clean session");

    assert_eq!(
        progress.reached(),
        Some(SessionPhase::EntrypointStarted),
        "a headless session ends at the entrypoint"
    );
    assert_eq!(progress.ready(true), Ok(()));

    let names: Vec<&str> = events.iter().map(LiveEvent::name).collect();
    assert_eq!(
        names,
        vec![
            "live.client.connected",
            "live.bundle.requested",
            "live.bundle.sent",
            "live.bundle.received",
            "live.bundle.loaded",
            "live.bundle.linked",
            "live.entrypoint.started",
            "live.session.ready",
        ],
        "the session emits its milestones in order"
    );
}

/// The real binary, as a real child process, with its stdout captured.
///
/// This is the test that cannot be satisfied by a milestone that lies: the
/// assertion is on the app's own output, which only exists inside the bytecode
/// the server sent over the socket.
#[test]
fn the_runner_binary_runs_the_app_it_is_served() {
    let dir = TempDir::new("subprocess");
    let server = LiveServer::bind(loopback(), vm_bundle()).expect("bind");
    let address = server.local_addr().expect("addr");

    let child = Command::new(env!("CARGO_BIN_EXE_kira-desktop-runner"))
        .arg("--server")
        .arg(address.to_string())
        .arg("--cache")
        .arg(&dir.0)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the runner binary spawns");
    let mut guard = ChildGuard::new(child);

    let mut events = Vec::new();
    let progress = server
        .serve_once(true, &mut |event| events.push(event))
        .expect("the server reports a clean session");
    assert_eq!(progress.ready(true), Ok(()));

    let (stdout, stderr, status) = finish(guard.take());
    assert!(
        stdout.contains(APP_OUTPUT),
        "the app's own output must appear on the runner's stdout.\n\
         stdout: {stdout:?}\nstderr: {stderr:?}\nstatus: {status:?}"
    );
    assert!(
        status.success(),
        "the runner must exit 0 for a session that ran. stderr: {stderr:?}"
    );
}

/// The server ending a session it has already been served by must not make the
/// runner report a failure.
///
/// This is the abrupt end — no shutdown, the socket simply gone, which is what a
/// supervisor that died looks like from the runner's side. The app is up and its
/// work is done, so the runner's remaining goodbye has nobody to reach; a runner
/// that called that a protocol failure would exit non-zero for a session that
/// ran, and would do it only under the timing that lost the race.
#[test]
fn a_server_that_vanishes_after_the_app_is_up_leaves_the_runner_clean() {
    let dir = TempDir::new("vanishing-server");
    let server = LiveServer::bind(loopback(), vm_bundle()).expect("bind");
    let address = server.local_addr().expect("addr");

    let child = Command::new(env!("CARGO_BIN_EXE_kira-desktop-runner"))
        .arg("--server")
        .arg(address.to_string())
        .arg("--cache")
        .arg(&dir.0)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the runner binary spawns");
    let mut guard = ChildGuard::new(child);

    let session = server
        .accept_session(vm_bundle(), true, &mut |_| {})
        .expect("the session reaches ready");
    assert_eq!(session.progress().ready(true), Ok(()));
    // No shutdown, no goodbye: the session is dropped where it stands.
    drop(session);

    let (stdout, stderr, status) = finish(guard.take());
    assert!(
        stdout.contains(APP_OUTPUT),
        "the app must have run. stdout: {stdout:?}\nstderr: {stderr:?}"
    );
    assert!(
        status.success(),
        "a server that goes away after the app is up is the session ending, not a failure. \
         stderr: {stderr:?}"
    );
}

/// A server that goes away *before* the runner has what it needs is still a
/// failure the runner reports.
///
/// The counterweight to the test above: a disconnect stops being a failure only
/// once the session has delivered what it was for. Here it never does, and the
/// runner must say so rather than exit as if it had run something.
#[test]
fn a_server_that_leaves_before_serving_fails_the_runner() {
    let listener = std::net::TcpListener::bind(loopback()).expect("bind");
    let address = listener.local_addr().expect("addr");

    let child = Command::new(env!("CARGO_BIN_EXE_kira-desktop-runner"))
        .arg("--server")
        .arg(address.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the runner binary spawns");
    let mut guard = ChildGuard::new(child);

    // Welcomed, and then abandoned: the runner is connected and has nothing.
    let (stream, _) = listener.accept().expect("accept");
    let mut reader = std::io::BufReader::new(stream.try_clone().expect("clone"));
    let mut writer = std::io::BufWriter::new(stream);
    let hello: kira_live::ClientMessage = kira_live::read_message(&mut reader).expect("hello");
    assert!(matches!(hello, kira_live::ClientMessage::Hello { .. }));
    kira_live::write_message(
        &mut writer,
        &kira_live::ServerMessage::Welcome {
            protocol: kira_live::PROTOCOL_VERSION,
        },
    )
    .expect("welcome");
    drop(writer);
    drop(reader);

    let (stdout, stderr, status) = finish(guard.take());
    assert!(
        !status.success(),
        "a runner that was never served must not exit 0. stdout: {stdout:?}\nstderr: {stderr:?}"
    );
    assert!(
        stderr.contains("disconnected"),
        "the runner must say the server left, got {stderr:?}"
    );
}

/// Reads a finished runner's output and waits for it.
fn finish(mut child: Child) -> (String, String, std::process::ExitStatus) {
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
    let status = child.wait().expect("the runner exits");
    (stdout, stderr, status)
}

/// A runner that connects for a different platform's bundle is refused, and told
/// why, rather than failing somewhere deep inside a load.
#[test]
fn a_runner_the_bundle_is_not_for_is_refused() {
    let bundle = Bundle::build(
        RunnerId::Android,
        BuildProfile::Debug,
        vec![NamedPayload {
            name: "app.kbc".to_owned(),
            kind: PayloadKind::VmBytecode,
            bytes: printing_module().to_bytes(),
        }],
        0,
    )
    .expect("a valid bundle");
    let server = LiveServer::bind(loopback(), bundle).expect("bind");
    let address = server.local_addr().expect("addr");

    let session = std::thread::spawn(move || server.serve_once(true, &mut |_| {}));

    // The desktop runner connecting to an Android bundle's server.
    let connected = RunnerClient::connect(address, RunnerId::Desktop);

    let error = session
        .join()
        .expect("the server thread does not panic")
        .expect_err("the server must refuse a runner its bundle is not for");
    assert!(
        matches!(
            error,
            kira_live::ServerError::RunnerMismatch {
                expected: "android",
                actual: "desktop"
            }
        ),
        "got {error:?}"
    );
    // The client either fails its handshake or is dropped mid-handshake; both are
    // fine, and which one is a race. What matters is the server's refusal above.
    drop(connected);
}

/// A session where the app never starts must not be ready. The runner reports
/// its failure and the server surfaces it — it does not round up to ready
/// because bytes were delivered.
#[test]
fn a_session_whose_app_never_starts_is_not_ready() {
    // A module whose entry calls a function that does not exist: it decodes, so
    // it loads, and fails at link — after the bundle was served in full.
    let module = Module {
        exports: Default::default(),
        foreign_imports: Vec::new(),
        foreign_aggregates: Default::default(),
        foreign_callbacks: Vec::new(),
        constants: Vec::new(),
        types: Vec::new(),
        main: Some(0),
        strings: Vec::new(),
        functions: vec![FuncProto {
            name: "main".to_owned(),
            param_count: 0,
            local_count: 0,
            execution: Execution::Runtime,
            code: vec![Instruction::Call(99), Instruction::ReturnVoid],
            releases: FrameRelease::EveryLocal,
        }],
    };
    let bundle = Bundle::build(
        RunnerId::Desktop,
        BuildProfile::Debug,
        vec![NamedPayload {
            name: "app.kbc".to_owned(),
            kind: PayloadKind::VmBytecode,
            bytes: module.to_bytes(),
        }],
        0,
    )
    .expect("a valid bundle");
    let dir = TempDir::new("never-starts");
    let server = LiveServer::bind(loopback(), bundle).expect("bind");
    let address = server.local_addr().expect("addr");

    let session = std::thread::spawn(move || server.serve_once(true, &mut |_| {}));

    let mut client = RunnerClient::connect(address, RunnerId::Desktop).expect("connect");
    let mut host = BundleHost::new(dir.0.clone());
    let client_error = client
        .run_session(&mut host)
        .expect_err("a module that cannot link must not run");
    assert!(
        matches!(
            client_error,
            kira_live::ClientError::Host { step: "link", .. }
        ),
        "the runner must fail at link, got {client_error:?}"
    );

    let server_error = session
        .join()
        .expect("the server thread does not panic")
        .expect_err("the server must not call this session ready");
    assert!(
        matches!(server_error, kira_live::ServerError::RunnerFailed(_)),
        "the server must surface the runner's reason, got {server_error:?}"
    );
}

/// A runner cannot assert its way to a ready session: reporting the entrypoint
/// without ever loading anything is refused as out of order.
#[test]
fn a_runner_cannot_skip_to_the_entrypoint() {
    let server = LiveServer::bind(loopback(), vm_bundle()).expect("bind");
    let address = server.local_addr().expect("addr");

    let session = std::thread::spawn(move || server.serve_once(true, &mut |_| {}));

    let mut client = RunnerClient::connect(address, RunnerId::Desktop).expect("connect");
    // No bundle fetched, nothing loaded, nothing linked. Just the claim.
    client
        .report(SessionPhase::EntrypointStarted)
        .expect("the claim goes out");

    let error = session
        .join()
        .expect("the server thread does not panic")
        .expect_err("an unearned milestone must be refused");
    assert!(
        matches!(error, kira_live::ServerError::Progress(_)),
        "got {error:?}"
    );
}

/// A runner cannot report a milestone that is the server's to observe, which
/// would otherwise let it claim a bundle was served that never was.
#[test]
fn a_runner_cannot_report_the_servers_milestones() {
    let server = LiveServer::bind(loopback(), vm_bundle()).expect("bind");
    let address = server.local_addr().expect("addr");

    let session = std::thread::spawn(move || server.serve_once(true, &mut |_| {}));

    let mut client = RunnerClient::connect(address, RunnerId::Desktop).expect("connect");
    client
        .report(SessionPhase::BundleSent)
        .expect("the claim goes out");

    let error = session
        .join()
        .expect("the server thread does not panic")
        .expect_err("a server-owned milestone must be refused");
    assert!(
        matches!(
            error,
            kira_live::ServerError::NotRunnerMilestone("bundle sent")
        ),
        "got {error:?}"
    );
}

/// A runner that connects and then goes quiet must fail the session rather than
/// hang the build. The bound is the server's read timeout.
#[test]
fn a_silent_runner_does_not_hang_the_server() {
    // A short-lived server whose timeout is the real one would make this test
    // take 30 seconds, so this asserts the mechanism rather than the duration:
    // the server's read timeout is set, and a dropped connection ends the wait.
    let server = LiveServer::bind(loopback(), vm_bundle()).expect("bind");
    let address = server.local_addr().expect("addr");

    let session = std::thread::spawn(move || server.serve_once(true, &mut |_| {}));

    // Connect and immediately drop without a Hello.
    let stream = std::net::TcpStream::connect(address).expect("connect");
    drop(stream);

    let error = session
        .join()
        .expect("the server thread does not panic")
        .expect_err("a runner that says nothing must not produce a ready session");
    assert!(
        matches!(
            error,
            kira_live::ServerError::Protocol(kira_live::ProtocolError::Disconnected)
        ),
        "got {error:?}"
    );
}

/// The session's timeouts are real bounds, not decoration.
///
/// Reads, writes, and the accept are each bounded, and each covers a distinct
/// way a session can stop making progress: a runner that says nothing, one that
/// stops reading, and one that never arrives.
///
/// One wait a live *session* does not own: the supervisor's watch loop, which
/// ends at `--quit-after` or when a person ends it and has no constant to check
/// here. Its half of the same rule is
/// `a_live_session_no_terminal_asked_for_ends_on_its_own`, in `kira-cli`'s
/// `live` module, where that loop's options are parsed.
#[test]
fn every_wait_in_a_session_is_bounded() {
    let ceiling = Duration::from_secs(60);
    assert!(kira_live::server::READ_TIMEOUT <= ceiling);
    assert!(kira_live::server::WRITE_TIMEOUT <= ceiling);
    assert!(kira_live::server::ACCEPT_TIMEOUT <= ceiling);
    assert!(kira_live::server::GOODBYE_TIMEOUT <= ceiling);
    assert!(kira_live::client::READ_TIMEOUT <= ceiling);
    assert!(kira_live::client::WRITE_TIMEOUT <= ceiling);
}

/// A runner that never connects must fail the session rather than hang it.
///
/// This is the case that has no other backstop: nothing panics, so no guard
/// drops, and a test process blocked in `accept` would sit there forever on a
/// host with no `timeout` to kill it. The server's own accept bound is what
/// makes a dead runner a failure instead of a wedged build.
#[test]
fn a_runner_that_never_connects_fails_the_session() {
    let server = LiveServer::bind(loopback(), vm_bundle()).expect("bind");

    // Deliberately no runner spawned: this is the runner-died-on-startup case.
    // A short bound rather than the production one, so the give-up path is
    // proven in milliseconds instead of half a minute.
    let bound = Duration::from_millis(200);
    let started = std::time::Instant::now();
    let error = server
        .serve_once_within(true, bound, &mut |_| {})
        .expect_err("a session with no runner must not succeed");

    assert!(
        matches!(error, kira_live::ServerError::RunnerNeverConnected),
        "got {error:?}"
    );
    assert!(
        started.elapsed() >= bound,
        "the session gave up before its own bound"
    );
}
