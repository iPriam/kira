//! The one error type the flow verbs return.
//!
//! Every failure is carried as a message that already reads as a `devflow:`
//! line, so `main` prints it once and exits non-zero. Subprocess failures are
//! formatted at the point they happen (command line + exit + stderr) rather
//! than wrapped, so the caller never has to reconstruct what git or gh said.

/// A devflow failure, ready to print.
#[derive(Debug, thiserror::Error)]
pub enum DevflowError {
    /// A human-facing failure. The `Display` prefixes it with `devflow: `.
    #[error("devflow: {0}")]
    Message(String),
    /// A wait verb hit its bound with the gate still pending. Distinct from
    /// `Message` only so a caller could branch on it; it prints the same way.
    #[error("devflow: {0}")]
    WaitTimedOut(String),
    /// No unresolved review thread matched the requested anchor. The caller
    /// turns this into the "already resolved, or line drifted" hint, which is
    /// why it is a variant rather than a plain message.
    #[error("devflow: no matching unresolved review thread")]
    ThreadNotFound,
}

impl DevflowError {
    /// Build a `Message` from anything string-like.
    pub fn msg(text: impl Into<String>) -> Self {
        Self::Message(text.into())
    }
}

impl From<std::io::Error> for DevflowError {
    fn from(error: std::io::Error) -> Self {
        Self::Message(error.to_string())
    }
}
