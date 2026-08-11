//! The Kira language server binary (`kira-language-server`).
//!
//! Speaks LSP over stdio via `lsp-server`, backed by the same salsa frontend
//! the compiler uses — so an editor squiggle and a `kira check` error are the
//! same computation, not two implementations that agree until they do not.
//!
//! # What it does today
//!
//! Diagnostics, on open and on every edit; go-to-definition and
//! go-to-declaration, served from the reference→definition links the
//! analyzer records as it resolves names — cross-file included; and hover and
//! completion built from those same links plus the parser's declaration tree.
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
mod features;

use lsp_server::{Connection, ExtractError, Message, Notification, Request};
use lsp_types::notification::{
    DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, Notification as _,
    PublishDiagnostics,
};
use lsp_types::request::{Completion, GotoDeclaration, GotoDefinition, HoverRequest, Request as _};
use lsp_types::{
    CompletionOptions, CompletionParams, CompletionResponse, DeclarationCapability,
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    GotoDefinitionParams, GotoDefinitionResponse, HoverParams, Location, OneOf,
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
        // Definition and declaration are one capability served two ways: Kira
        // has no separate declarations (no headers, no forward declares), so
        // both jumps land on the same name token.
        definition_provider: Some(OneOf::Left(true)),
        declaration_provider: Some(DeclarationCapability::Simple(true)),
        hover_provider: Some(lsp_types::HoverProviderCapability::Simple(true)),
        completion_provider: Some(CompletionOptions {
            // A dot changes the completion context from names in scope to
            // members. The current catalog is deliberately conservative and
            // still returns useful names for Ctrl+Space everywhere.
            trigger_characters: Some(vec![".".to_owned()]),
            ..CompletionOptions::default()
        }),
        ..ServerCapabilities::default()
    })?;

    let (id, _params) = connection.initialize_start()?;
    connection.initialize_finish(
        id,
        serde_json::json!({
            "capabilities": capabilities,
            "serverInfo": {
                "name": "kira-language-server",
                "version": kira_toolchain::RELEASE_VERSION,
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
    // One session for the life of the server: see `analysis::AnalysisSession`.
    let mut session = analysis::AnalysisSession::new();

    for message in &connection.receiver {
        match message {
            Message::Request(request) => {
                // `shutdown` first: `handle_shutdown` replies to it itself.
                if connection.handle_shutdown(&request)? {
                    return Ok(());
                }
                match request.method.as_str() {
                    // Declaration and definition are the same jump here: Kira
                    // declares nothing separately from where it defines it.
                    GotoDefinition::METHOD | GotoDeclaration::METHOD => {
                        let id = request.id.clone();
                        let response = match extract_request::<GotoDefinitionParams>(request) {
                            Ok(params) => lsp_server::Response::new_ok(
                                id,
                                definition(&mut session, &documents, &params),
                            ),
                            Err(error) => lsp_server::Response::new_err(
                                id,
                                lsp_server::ErrorCode::InvalidParams as i32,
                                error.to_string(),
                            ),
                        };
                        connection.sender.send(Message::Response(response))?;
                    }
                    HoverRequest::METHOD => {
                        let id = request.id.clone();
                        let response = match extract_request::<HoverParams>(request) {
                            Ok(params) => lsp_server::Response::new_ok(
                                id,
                                features::hover(&mut session, &documents, &params),
                            ),
                            Err(error) => lsp_server::Response::new_err(
                                id,
                                lsp_server::ErrorCode::InvalidParams as i32,
                                error.to_string(),
                            ),
                        };
                        connection.sender.send(Message::Response(response))?;
                    }
                    Completion::METHOD => {
                        let id = request.id.clone();
                        let response = match extract_request::<CompletionParams>(request) {
                            Ok(params) => lsp_server::Response::new_ok(
                                id,
                                CompletionResponse::Array(features::completion(
                                    &mut session,
                                    &documents,
                                    &params,
                                )),
                            ),
                            Err(error) => lsp_server::Response::new_err(
                                id,
                                lsp_server::ErrorCode::InvalidParams as i32,
                                error.to_string(),
                            ),
                        };
                        connection.sender.send(Message::Response(response))?;
                    }
                    // Anything else is a capability this server never
                    // advertised. The protocol has an answer for that;
                    // inventing a reply would be worse than admitting the gap.
                    _ => {
                        let response = lsp_server::Response::new_err(
                            request.id,
                            lsp_server::ErrorCode::MethodNotFound as i32,
                            format!("kira-language-server does not handle `{}`", request.method),
                        );
                        connection.sender.send(Message::Response(response))?;
                    }
                }
            }
            Message::Notification(notification) => {
                notify(&connection, &mut session, &mut documents, notification)?;
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
    session: &mut analysis::AnalysisSession,
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
            publish(connection, session, documents, &params.text_document.uri)?;
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
                publish(connection, session, documents, &params.text_document.uri)?;
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

/// The path a document is analyzed under: the real one when the URI names a
/// file — which is what lets `import support` resolve beside it and lets a
/// cross-file jump name the right file — or the display name when it does not.
fn analysis_path(uri: &Uri) -> String {
    documents::file_path(uri).unwrap_or_else(|| documents::display_name(uri))
}

/// Analyzes a document and publishes what the frontend said about it.
fn publish(
    connection: &Connection,
    session: &mut analysis::AnalysisSession,
    documents: &Documents,
    uri: &Uri,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(text) = documents.text(uri) else {
        // A change to a document that was never opened is a client bug, not
        // something to analyze a stale copy for.
        return Ok(());
    };

    let analysis = analysis::analyze(session, &analysis_path(uri), text);
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

/// Answers a definition (or declaration) request from a fresh analysis.
///
/// The cursor names a reference when it sits inside one of the identifier
/// spans the analyzer linked; the answer is that link's definition, in
/// whichever file it lives. `None` — a `null` reply — is the protocol's way
/// of saying "nothing to jump to", and it is the honest answer for a
/// position on whitespace, a keyword, or a name that never resolved.
fn definition(
    session: &mut analysis::AnalysisSession,
    documents: &Documents,
    params: &GotoDefinitionParams,
) -> GotoDefinitionResponse {
    let uri = &params.text_document_position_params.text_document.uri;
    let position = params.text_document_position_params.position;
    let Some(text) = documents.text(uri) else {
        return GotoDefinitionResponse::Array(Vec::new());
    };

    let analysis = analysis::analyze(session, &analysis_path(uri), text);
    let offset = convert::offset(&analysis.file, position);
    let Some(link) = reference_at(&analysis, offset) else {
        return GotoDefinitionResponse::Array(Vec::new());
    };

    let target = &analysis.files[link.definition.source.value() as usize];
    let target_uri = if link.definition.source == analysis::DOCUMENT_SOURCE {
        Some(uri.clone())
    } else {
        documents::path_uri(&target.path)
    };
    match target_uri {
        Some(target_uri) => GotoDefinitionResponse::Scalar(Location {
            uri: target_uri,
            range: convert::range(target, link.definition.span),
        }),
        None => GotoDefinitionResponse::Array(Vec::new()),
    }
}

/// The most specific link whose reference span contains `offset` in the
/// document.
///
/// The end is inclusive so a cursor sitting just after the last character of
/// a name still jumps — that is where a double-click leaves it. Ties go to
/// the shortest span, so a name inside a larger linked region answers for
/// itself.
fn reference_at(
    analysis: &analysis::Analysis,
    offset: u32,
) -> Option<kira_semantics::DefinitionLink> {
    analysis
        .definitions
        .iter()
        .copied()
        .filter(|link| {
            link.reference.source == analysis::DOCUMENT_SOURCE
                && link.reference.span.start <= offset
                && offset <= link.reference.span.end()
        })
        .min_by_key(|link| link.reference.span.len)
}

/// Deserializes a request's parameters, naming the method on failure.
fn extract_request<P: serde::de::DeserializeOwned>(
    request: Request,
) -> Result<P, Box<dyn std::error::Error>> {
    let method = request.method.clone();
    request
        .extract::<P>(&method)
        .map(|(_, params)| params)
        .map_err(|error| -> Box<dyn std::error::Error> {
            match error {
                ExtractError::JsonError { method, error } => {
                    format!("malformed `{method}` parameters: {error}").into()
                }
                ExtractError::MethodMismatch(request) => {
                    format!("unexpected method `{}`", request.method).into()
                }
            }
        })
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
#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::{
        PartialResultParams, Position, TextDocumentIdentifier, WorkDoneProgressParams,
    };
    use std::str::FromStr as _;

    fn request_at(uri: &Uri, position: Position) -> GotoDefinitionParams {
        GotoDefinitionParams {
            text_document_position_params: lsp_types::TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
                position,
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        }
    }

    /// The whole request path: a cursor on a local's read jumps to its `let`.
    #[test]
    fn a_definition_request_jumps_to_the_binding() {
        let uri = Uri::from_str("file:///tmp/kira_lsp_def_test.kira").expect("valid uri");
        let text = "@Main function main() { let value = 1 print(value) return }";
        let mut documents = Documents::new();
        documents.set(&uri, text.to_owned(), 1);

        // The cursor sits on the `value` inside `print(...)`, column 44.
        let read = text.rfind("value").expect("the read is there") as u32;
        let response = definition(
            &mut analysis::AnalysisSession::new(),
            &documents,
            &request_at(&uri, Position::new(0, read)),
        );
        let GotoDefinitionResponse::Scalar(location) = response else {
            panic!("a resolvable name answers with one location, got {response:?}");
        };
        assert_eq!(location.uri, uri);
        let binding = text.find("value").expect("the binding is there") as u32;
        assert_eq!(location.range.start, Position::new(0, binding));
        assert_eq!(
            location.range.end,
            Position::new(0, binding + "value".len() as u32)
        );
    }

    /// A jump across files: the definition names the module's own URI.
    #[test]
    fn a_definition_request_crosses_into_an_imported_module() {
        let directory = std::env::temp_dir().join(format!(
            "kira_lsp_def_module_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&directory).expect("temp dir");
        let module_path = directory.join("support.kira");
        let module_text = "function supportValue() -> Int { return 42 }";
        std::fs::write(&module_path, module_text).expect("write module");
        let entry = directory.join("main.kira");
        let text = "import support as Support\n\
                    @Main function main() { print(Support.supportValue()) return }";
        let uri = documents::path_uri(&entry.to_string_lossy()).expect("a file uri");
        let mut documents = Documents::new();
        documents.set(&uri, text.to_owned(), 1);

        let line1 = text.lines().nth(1).expect("two lines");
        let call = line1.find("supportValue").expect("the call is there") as u32;
        let response = definition(
            &mut analysis::AnalysisSession::new(),
            &documents,
            &request_at(&uri, Position::new(1, call)),
        );
        let GotoDefinitionResponse::Scalar(location) = response else {
            panic!("a cross-module name answers with one location, got {response:?}");
        };
        assert_eq!(
            location.uri,
            documents::path_uri(&module_path.to_string_lossy()).expect("a file uri"),
            "the jump lands in the module's file"
        );
        let declaration = module_text.find("supportValue").expect("declared") as u32;
        assert_eq!(location.range.start, Position::new(0, declaration));

        let _ = std::fs::remove_dir_all(&directory);
    }

    /// Whitespace has nothing to jump to, and the reply says so rather than
    /// erroring.
    #[test]
    fn a_position_on_nothing_answers_with_no_locations() {
        let uri = Uri::from_str("file:///tmp/kira_lsp_def_none.kira").expect("valid uri");
        let text = "@Main function main() { return }";
        let mut documents = Documents::new();
        documents.set(&uri, text.to_owned(), 1);

        let response = definition(
            &mut analysis::AnalysisSession::new(),
            &documents,
            &request_at(&uri, Position::new(0, 22)),
        );
        assert!(
            matches!(response, GotoDefinitionResponse::Array(ref locations) if locations.is_empty()),
            "got {response:?}"
        );
    }
}
