//! End-to-end tests: drive the real server binary over stdio, as an editor does.
//!
//! The unit tests prove the analysis and the span conversion. They cannot prove
//! the thing an editor actually depends on: that the binary speaks the protocol
//! — frames its messages, completes the handshake, and pushes diagnostics
//! unprompted. So these spawn `kira-language-server` and talk LSP to it.
//!
//! The client here is hand-rolled rather than a library, because the framing is
//! exactly what is under test: a test that used the same crate the server does
//! to write *and* read would agree with itself about a wrong format.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

/// A running server and the pipes to it.
///
/// `stdin` is an `Option` because closing it is load-bearing, not cleanup: the
/// server's reader thread blocks until stdin reaches EOF, which is what an
/// editor causes by closing the pipe as it exits. A client that sends `exit`
/// but holds the pipe open leaves the server waiting forever — so [`Self::shutdown`]
/// drops it explicitly, and [`Drop`] guarantees it even when a test panics
/// first.
struct Server {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
}

impl Drop for Server {
    fn drop(&mut self) {
        // A panicking test must not leave a server behind holding a pipe: the
        // next `cargo test` run would inherit it, and a leaked one hangs rather
        // than fails.
        drop(self.stdin.take());
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Server {
    /// Spawns the server and completes the initialize handshake.
    fn start() -> Server {
        let mut child = Command::new(env!("CARGO_BIN_EXE_kira-language-server"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn the language server");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        let mut server = Server {
            child,
            stdin: Some(stdin),
            stdout,
        };

        server.send(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "capabilities": {} },
        }));
        let response = server.read();
        assert_eq!(response["id"], 1, "the handshake is answered: {response}");
        assert!(
            response["result"]["capabilities"]["textDocumentSync"].is_number(),
            "the server advertises document sync: {response}",
        );

        server.send(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": "initialized",
            "params": {},
        }));
        server
    }

    /// Writes one LSP-framed message.
    fn send(&mut self, message: &serde_json::Value) {
        let body = serde_json::to_string(message).expect("serialize");
        let stdin = self.stdin.as_mut().expect("stdin is open until shutdown");
        write!(stdin, "Content-Length: {}\r\n\r\n{body}", body.len()).expect("write");
        stdin.flush().expect("flush");
    }

    /// Reads one LSP-framed message.
    fn read(&mut self) -> serde_json::Value {
        let mut length = None;
        loop {
            let mut line = String::new();
            let read = self.stdout.read_line(&mut line).expect("read header");
            assert_ne!(read, 0, "the server closed the connection mid-message");
            let trimmed = line.trim_end();
            if trimmed.is_empty() {
                break;
            }
            if let Some(value) = trimmed.strip_prefix("Content-Length: ") {
                length = Some(value.parse::<usize>().expect("a numeric Content-Length"));
            }
        }
        let length = length.expect("every message is framed with a Content-Length");
        let mut body = vec![0; length];
        self.stdout.read_exact(&mut body).expect("read body");
        serde_json::from_slice(&body).expect("a JSON body")
    }

    /// Opens a document and returns the diagnostics the server pushes for it.
    fn open(&mut self, uri: &str, text: &str) -> serde_json::Value {
        self.send(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": uri,
                    "languageId": "kira",
                    "version": 1,
                    "text": text,
                },
            },
        }));
        self.await_diagnostics(uri)
    }

    /// Replaces a document's text and returns the diagnostics that follow.
    fn change(&mut self, uri: &str, text: &str, version: i64) -> serde_json::Value {
        self.send(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didChange",
            "params": {
                "textDocument": { "uri": uri, "version": version },
                "contentChanges": [{ "text": text }],
            },
        }));
        self.await_diagnostics(uri)
    }

    /// Reads until the server publishes diagnostics for `uri`.
    fn await_diagnostics(&mut self, uri: &str) -> serde_json::Value {
        for _ in 0..16 {
            let message = self.read();
            if message["method"] == "textDocument/publishDiagnostics"
                && message["params"]["uri"] == uri
            {
                return message["params"]["diagnostics"].clone();
            }
        }
        panic!("the server never published diagnostics for {uri}");
    }

    /// Shuts the server down and asserts it exits cleanly.
    fn shutdown(mut self) {
        self.send(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 99,
            "method": "shutdown",
            "params": null,
        }));
        let response = self.read();
        assert_eq!(response["id"], 99, "shutdown is answered: {response}");

        self.send(&serde_json::json!({ "jsonrpc": "2.0", "method": "exit" }));
        // Closing stdin is what lets the server's reader thread finish; an
        // editor does this by exiting. Without it, `wait` below deadlocks.
        drop(self.stdin.take());

        let status = self.child.wait().expect("the server exits");
        assert!(
            status.success(),
            "a server told to shut down exits cleanly, got {status}",
        );
    }
}

/// A program with a real error must produce a real squiggle: the right code, at
/// the right place, marked as an error.
#[test]
fn an_error_is_published_with_its_code_and_span() {
    let mut server = Server::start();
    let uri = "file:///tmp/kira-lsp-test/broken.kira";
    let diagnostics = server.open(
        uri,
        "@Main\nfunction main() {\n    print(missing)\n    return\n}\n",
    );

    let first = diagnostics
        .as_array()
        .expect("an array of diagnostics")
        .iter()
        .find(|diagnostic| diagnostic["code"] == "KSEM060")
        .unwrap_or_else(|| panic!("the unknown name is reported: {diagnostics}"));

    assert_eq!(first["severity"], 1, "an error, not a warning");
    assert_eq!(first["source"], "kira");
    // `missing` is on the third line (0-based line 2), inside `print(...)`.
    assert_eq!(first["range"]["start"]["line"], 2, "{first}");
    assert_eq!(first["range"]["start"]["character"], 10, "{first}");
    assert_eq!(first["range"]["end"]["character"], 17, "{first}");

    server.shutdown();
}

/// A clean program must publish an *empty* list, not silence: that is how the
/// client is told to clear the previous squiggles.
#[test]
fn a_clean_program_publishes_an_empty_list() {
    let mut server = Server::start();
    let diagnostics = server.open(
        "file:///tmp/kira-lsp-test/clean.kira",
        "@Main\nfunction main() {\n    print(1)\n    return\n}\n",
    );
    assert_eq!(
        diagnostics.as_array().map(Vec::len),
        Some(0),
        "a clean program has no diagnostics, got {diagnostics}",
    );
    server.shutdown();
}

/// The loop an editor actually drives: type something broken, see the error;
/// fix it, watch it clear. Diagnostics must track the buffer, not the disk.
#[test]
fn diagnostics_follow_edits_and_clear_when_fixed() {
    let mut server = Server::start();
    let uri = "file:///tmp/kira-lsp-test/edited.kira";

    let broken = server.open(
        uri,
        "@Main\nfunction main() {\n    print(nope)\n    return\n}\n",
    );
    assert!(
        !broken.as_array().expect("an array").is_empty(),
        "the broken buffer reports: {broken}",
    );

    let fixed = server.change(
        uri,
        "@Main\nfunction main() {\n    print(1)\n    return\n}\n",
        2,
    );
    assert_eq!(
        fixed.as_array().map(Vec::len),
        Some(0),
        "fixing the buffer clears the squiggles, got {fixed}",
    );

    // And back again: the server holds no stale state between edits.
    let broken_again = server.change(
        uri,
        "@Main\nfunction main() {\n    print(nope)\n    return\n}\n",
        3,
    );
    assert!(
        !broken_again.as_array().expect("an array").is_empty(),
        "re-breaking the buffer reports again: {broken_again}",
    );

    server.shutdown();
}

/// The server must survive a request it never advertised, rather than dying and
/// taking the editor's language support with it.
#[test]
fn an_unsupported_request_is_refused_without_killing_the_server() {
    let mut server = Server::start();
    server.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 7,
        "method": "textDocument/hover",
        "params": {
            "textDocument": { "uri": "file:///tmp/kira-lsp-test/x.kira" },
            "position": { "line": 0, "character": 0 },
        },
    }));
    let response = server.read();
    assert_eq!(response["id"], 7);
    assert_eq!(
        response["error"]["code"], -32601,
        "an unhandled method is MethodNotFound: {response}",
    );

    // Still alive and still analyzing.
    let diagnostics = server.open(
        "file:///tmp/kira-lsp-test/after.kira",
        "@Main\nfunction main() {\n    print(1)\n    return\n}\n",
    );
    assert_eq!(diagnostics.as_array().map(Vec::len), Some(0));
    server.shutdown();
}

/// New syntax reaches the editor by construction: the server serves the same
/// salsa `analyzed` query `kirac check` does, so a `type` alias needs no LSP
/// change to be understood. This is the test that says so — an aliased program
/// is clean, and a cyclic alias squiggles with the code semantics gave it.
#[test]
fn type_aliases_reach_the_editor_through_the_shared_frontend() {
    let mut server = Server::start();
    let clean = server.open(
        "file:///tmp/kira-lsp-test/aliases.kira",
        "type Count = Int\ntype Buffer = [Count]\n@Main\nfunction main() {\n    \
         var xs: Buffer = []\n    xs.append(1)\n    let n: Count = xs.count\n    \
         print(n)\n    return\n}\n",
    );
    assert_eq!(
        clean.as_array().map(Vec::len),
        Some(0),
        "an aliased program is clean, got {clean}",
    );

    let cyclic = server.open(
        "file:///tmp/kira-lsp-test/cyclic.kira",
        "type A = B\ntype B = A\n@Main\nfunction main() {\n    let x: A = 1\n    \
         print(x)\n    return\n}\n",
    );
    assert!(
        cyclic
            .as_array()
            .expect("an array of diagnostics")
            .iter()
            .any(|diagnostic| diagnostic["code"] == "KSEM157"),
        "the alias cycle is reported to the editor: {cyclic}",
    );

    server.shutdown();
}

/// Closures reach the editor by construction, for the same reason type aliases
/// do: the server serves the same salsa `analyzed` query `kirac check` does, so
/// a closure needs no LSP change to be understood. A closure program is clean,
/// and a refused capture squiggles with the code semantics gave it.
#[test]
fn closures_reach_the_editor_through_the_shared_frontend() {
    let mut server = Server::start();
    let clean = server.open(
        "file:///tmp/kira-lsp-test/closures.kira",
        "function apply(f: borrow (Int) -> Int, x: Int) -> Int {\n    return f(x)\n}\n\
         @Main\nfunction main() {\n    let step = 2\n    \
         let bump: (Int) -> Int = { v in return v + step }\n    \
         print(apply(bump, 1))\n    return\n}\n",
    );
    assert_eq!(
        clean.as_array().map(Vec::len),
        Some(0),
        "a closure program is clean, got {clean}",
    );

    let captured_var = server.open(
        "file:///tmp/kira-lsp-test/capture.kira",
        "function run(f: () -> Int) -> Int {\n    return f()\n}\n\
         @Main\nfunction main() {\n    var total = 0\n    \
         print(run { in return total })\n    return\n}\n",
    );
    assert!(
        captured_var
            .as_array()
            .expect("an array of diagnostics")
            .iter()
            .any(|diagnostic| diagnostic["code"] == "KSEM117"),
        "the refused capture is reported to the editor: {captured_var}",
    );

    server.shutdown();
}
