//! The Kira language server binary (`kira-language-server`).
//!
//! Speaks LSP over stdio via `lsp-server`, backed by the same salsa frontend
//! the compiler uses — so an editor squiggle and a `kirac check` error are the
//! same computation, not two implementations that agree until they do not.
//!
//! # What it does today
//!
//! Diagnostics, on open and on every edit. That is the whole of it: v0 has no
//! cross-file resolution, so hover, go-to-definition, and completion are the
//! next things to build on this transport rather than things it stubs out.
//!
//! # Shape
//!
//! Single-threaded and synchronous. Analysis of a one-file v0 program is
//! microseconds, so there is nothing to move off the main loop yet; a
//! cancellation and worker-thread design is what this grows when analysis grows
//! teeth, and pretending to need one now would be architecture without a
//! problem.
//!
//! stdout is the protocol's transport. Nothing here may `println!` — a stray
//! byte on stdout is a protocol violation that desynchronizes the client.
//! Reports about the *server* go to stderr, which editors capture as logs.

mod analysis;
mod convert;
mod documents;

use lsp_server::{Connection, ExtractError, Message, Notification};
use lsp_types::notification::{
    DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, Notification as _,
    PublishDiagnostics,
};
use lsp_types::{
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    PublishDiagnosticsParams, ServerCapabilities, TextDocumentSyncCapability, TextDocumentSyncKind,
    Uri,
};

use documents::Documents;

/// Exit code for a server that could not start or could not keep talking.
const EXIT_FAILURE: i32 = 1;

fn main() {
    if let Err(error) = run() {
        eprintln!("kira-language-server: {error}");
        std::process::exit(EXIT_FAILURE);
    }
}

/// Serves LSP over stdio until the client shuts the server down.
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let (connection, io_threads) = Connection::stdio();

    let capabilities = serde_json::to_value(ServerCapabilities {
        // Full-text sync: the client resends the whole document on every edit.
        // Incremental sync would save bytes the analysis does not care about —
        // a v0 program is one small file, and reassembling ranges correctly is
        // a real source of bugs to take on for nothing.
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        ..ServerCapabilities::default()
    })?;

    let (id, _params) = connection.initialize_start()?;
    connection.initialize_finish(
        id,
        serde_json::json!({
            "capabilities": capabilities,
            "serverInfo": {
                "name": "kira-language-server",
                "version": env!("CARGO_PKG_VERSION"),
            },
        }),
    )?;

    // `serve` takes the connection **by value** so it is dropped before the
    // join below, and that is load-bearing rather than tidy: `io_threads.join`
    // waits on the writer thread as well as the reader, and the writer only
    // finishes once every sender is gone. Holding the connection across the
    // join deadlocks a server that has already been told to shut down — it
    // answers `shutdown`, stops reading, and then never exits.
    serve(connection)?;
    io_threads.join()?;
    Ok(())
}

/// The main loop: route each message until the client says to stop.
///
/// Consumes the connection: see [`run`] for why dropping it before joining the
/// I/O threads is what lets the process exit.
fn serve(connection: Connection) -> Result<(), Box<dyn std::error::Error>> {
    let mut documents = Documents::new();

    for message in &connection.receiver {
        match message {
            Message::Request(request) => {
                // The only request this server answers is `shutdown`, which
                // `handle_shutdown` replies to itself.
                if connection.handle_shutdown(&request)? {
                    return Ok(());
                }
                // Anything else is a capability this server never advertised.
                // The protocol has an answer for that; inventing a reply would
                // be worse than admitting the gap.
                let response = lsp_server::Response::new_err(
                    request.id,
                    lsp_server::ErrorCode::MethodNotFound as i32,
                    format!("kira-language-server does not handle `{}`", request.method),
                );
                connection.sender.send(Message::Response(response))?;
            }
            Message::Notification(notification) => {
                notify(&connection, &mut documents, notification)?;
            }
            // Responses to requests this server never sends.
            Message::Response(_) => {}
        }
    }
    Ok(())
}

/// Handles one notification, republishing diagnostics when the text changed.
fn notify(
    connection: &Connection,
    documents: &mut Documents,
    notification: Notification,
) -> Result<(), Box<dyn std::error::Error>> {
    match notification.method.as_str() {
        DidOpenTextDocument::METHOD => {
            let params: DidOpenTextDocumentParams = extract(notification)?;
            documents.set(
                &params.text_document.uri,
                params.text_document.text,
                params.text_document.version,
            );
            publish(connection, documents, &params.text_document.uri)?;
        }
        DidChangeTextDocument::METHOD => {
            let params: DidChangeTextDocumentParams = extract(notification)?;
            // Full sync, so the last change carries the whole document.
            if let Some(change) = params.content_changes.into_iter().next_back() {
                documents.set(
                    &params.text_document.uri,
                    change.text,
                    params.text_document.version,
                );
                publish(connection, documents, &params.text_document.uri)?;
            }
        }
        DidCloseTextDocument::METHOD => {
            let params: DidCloseTextDocumentParams = extract(notification)?;
            documents.remove(&params.text_document.uri);
            // A closed document's squiggles are the client's to forget, and it
            // only does so if told: clear them explicitly.
            send_diagnostics(connection, &params.text_document.uri, Vec::new(), None)?;
        }
        // Every other notification is one this server does not act on.
        // Notifications take no reply, so ignoring is the correct handling.
        _ => {}
    }
    Ok(())
}

/// Analyzes a document and publishes what the frontend said about it.
fn publish(
    connection: &Connection,
    documents: &Documents,
    uri: &Uri,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(text) = documents.text(uri) else {
        // A change to a document that was never opened is a client bug, not
        // something to analyze a stale copy for.
        return Ok(());
    };

    let analysis = analysis::analyze(&documents::display_name(uri), text);
    let diagnostics = analysis
        .diagnostics
        .iter()
        .map(|diagnostic| convert::diagnostic(diagnostic, &analysis.file, uri))
        .collect();
    send_diagnostics(connection, uri, diagnostics, documents.version(uri))
}

/// Sends one `publishDiagnostics` notification.
///
/// An empty list is meaningful: it is how a client is told the file is clean.
///
/// `version` is the revision of the text these diagnostics describe, and it
/// matters more than its optionality suggests: it is how a client knows the
/// spans still line up with the buffer it is holding. Without it a client has
/// no way to distinguish a fresh diagnostic from one computed two keystrokes
/// ago, and is entitled to treat the range as untrustworthy rather than
/// underline text that may have shifted.
fn send_diagnostics(
    connection: &Connection,
    uri: &Uri,
    diagnostics: Vec<lsp_types::Diagnostic>,
    version: Option<i32>,
) -> Result<(), Box<dyn std::error::Error>> {
    let params = PublishDiagnosticsParams {
        uri: uri.clone(),
        diagnostics,
        version,
    };
    let notification = Notification::new(PublishDiagnostics::METHOD.to_owned(), params);
    connection
        .sender
        .send(Message::Notification(notification))?;
    Ok(())
}

/// Deserializes a notification's parameters, naming the method on failure.
fn extract<P: serde::de::DeserializeOwned>(
    notification: Notification,
) -> Result<P, Box<dyn std::error::Error>> {
    let method = notification.method.clone();
    notification
        .extract::<P>(&method)
        .map_err(|error| -> Box<dyn std::error::Error> {
            match error {
                ExtractError::JsonError { method, error } => {
                    format!("malformed `{method}` parameters: {error}").into()
                }
                ExtractError::MethodMismatch(notification) => {
                    format!("unexpected method `{}`", notification.method).into()
                }
            }
        })
}
