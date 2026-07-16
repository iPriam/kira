//! The open documents, and the text the server analyzes.
//!
//! A language server analyzes what the *editor* holds, not what is on disk:
//! those differ from the first unsaved keystroke, and the unsaved buffer is the
//! one a user wants squiggles for. So the text lives here, keyed by URI, and
//! disk is never read for an open document.
//!
//! # Versions
//!
//! Each document carries the version the editor stamped it with. That version
//! travels back out on every `publishDiagnostics`, which is what lets a client
//! prove a diagnostic describes the text it currently holds rather than a
//! keystroke ago. A client that cannot prove that is entitled to distrust the
//! diagnostic's range — the text may have moved under it — and to decline to
//! draw it against the buffer.

use std::collections::HashMap;

use lsp_types::Uri;

/// One open document: what the editor holds, and which revision it is.
#[derive(Debug, Clone)]
struct Document {
    text: String,
    version: i32,
}

/// The documents the editor currently has open.
#[derive(Debug, Default)]
pub struct Documents {
    open: HashMap<String, Document>,
}

impl Documents {
    /// An empty store.
    pub fn new() -> Documents {
        Documents::default()
    }

    /// Records a document's full text at `version`, replacing any previous one.
    pub fn set(&mut self, uri: &Uri, text: String, version: i32) {
        self.open.insert(key(uri), Document { text, version });
    }

    /// Forgets a document the editor closed.
    pub fn remove(&mut self, uri: &Uri) {
        self.open.remove(&key(uri));
    }

    /// The text of an open document, or `None` when the editor never opened it.
    pub fn text(&self, uri: &Uri) -> Option<&str> {
        self.open
            .get(&key(uri))
            .map(|document| document.text.as_str())
    }

    /// The version the editor last stamped an open document with.
    pub fn version(&self, uri: &Uri) -> Option<i32> {
        self.open.get(&key(uri)).map(|document| document.version)
    }
}

/// The store's key for a URI.
fn key(uri: &Uri) -> String {
    uri.as_str().to_owned()
}

/// A readable file name for a URI, for diagnostics that name their file.
///
/// Best-effort and cosmetic: the URI's last path segment, or the whole URI when
/// it has no path. Nothing resolves a real filesystem path from this — the
/// server analyzes the editor's buffer, and the name is only ever displayed.
pub fn display_name(uri: &Uri) -> String {
    let text = uri.as_str();
    match text.rsplit_once('/') {
        Some((_, name)) if !name.is_empty() => name.to_owned(),
        _ => text.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn uri(text: &str) -> Uri {
        Uri::from_str(text).expect("a valid uri")
    }

    #[test]
    fn a_document_round_trips_and_the_latest_text_wins() {
        let mut documents = Documents::new();
        let uri = uri("file:///tmp/a.kira");
        assert_eq!(documents.text(&uri), None);
        assert_eq!(documents.version(&uri), None);

        documents.set(&uri, "first".to_owned(), 1);
        assert_eq!(documents.text(&uri), Some("first"));
        assert_eq!(documents.version(&uri), Some(1));

        documents.set(&uri, "second".to_owned(), 2);
        assert_eq!(documents.text(&uri), Some("second"));
        assert_eq!(documents.version(&uri), Some(2));

        documents.remove(&uri);
        assert_eq!(documents.text(&uri), None);
        assert_eq!(documents.version(&uri), None);
    }

    /// The version and the text move together: a client uses the version to
    /// prove a diagnostic's spans describe the text it is holding, so a version
    /// that outlived its text would be worse than none at all.
    #[test]
    fn a_version_never_outlives_the_text_it_describes() {
        let mut documents = Documents::new();
        let uri = uri("file:///tmp/a.kira");
        documents.set(&uri, "first".to_owned(), 1);
        documents.set(&uri, "second".to_owned(), 2);
        assert_eq!(
            (documents.text(&uri), documents.version(&uri)),
            (Some("second"), Some(2)),
        );
    }

    #[test]
    fn documents_are_independent() {
        let mut documents = Documents::new();
        documents.set(&uri("file:///tmp/a.kira"), "a".to_owned(), 1);
        documents.set(&uri("file:///tmp/b.kira"), "b".to_owned(), 1);
        assert_eq!(documents.text(&uri("file:///tmp/a.kira")), Some("a"));
        assert_eq!(documents.text(&uri("file:///tmp/b.kira")), Some("b"));
    }

    #[test]
    fn a_display_name_is_the_last_path_segment() {
        assert_eq!(display_name(&uri("file:///tmp/demo.kira")), "demo.kira");
        assert_eq!(
            display_name(&uri("untitled:Untitled-1")),
            "untitled:Untitled-1"
        );
    }
}
