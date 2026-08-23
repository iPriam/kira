//! The sessions this server is holding open.
//!
//! A debug session outlives the call that started it — that is the whole
//! reason this server exists rather than a `kira debug` invocation per
//! question. Every session therefore has a name the caller keeps using, and
//! the server owns the process until the caller closes it or the server exits.

use kira_debug::PreparedTarget;
use serde_json::{Value, json};

use crate::report::session_summary;
use crate::session::Session;

/// Every open session, in the order they were started.
#[derive(Default)]
pub struct Sessions {
    open: Vec<Session>,
    next: u32,
}

impl Sessions {
    /// Starts a session over `target` and returns its identifier.
    ///
    /// A start that fails — no adapter installed, a launch that never came up
    /// — still owns `target`, so its build artifacts are removed here rather
    /// than left to accumulate one per failed attempt.
    pub fn open(&mut self, target: PreparedTarget) -> Result<&mut Session, String> {
        self.next += 1;
        let id = format!("s{}", self.next);
        match Session::start(id, target) {
            Ok(session) => {
                self.open.push(session);
                self.open
                    .last_mut()
                    .ok_or_else(|| "the session was lost while being started".to_owned())
            }
            // `Session::start` consumed the target on success only; on
            // failure it hands it back so the artifacts can be removed here.
            Err(failure) => {
                failure.target.clean();
                Err(failure.reason)
            }
        }
    }

    /// The session `id` names.
    pub fn get(&mut self, id: &str) -> Result<&mut Session, String> {
        let known = self.names();
        self.open
            .iter_mut()
            .find(|session| session.id == id)
            .ok_or_else(|| match known.is_empty() {
                true => format!("no session `{id}`; none are open"),
                false => format!("no session `{id}`; open sessions are {}", known.join(", ")),
            })
    }

    /// The only open session, when a caller named none.
    ///
    /// Naming a session is optional while there is exactly one, because that
    /// is the common case and the identifier carries no information then. With
    /// several open it becomes required, rather than one of them being picked.
    pub fn only(&mut self) -> Result<&mut Session, String> {
        match self.open.len() {
            0 => Err("no debug session is open; start one with `kira_lldb_launch`".to_owned()),
            1 => Ok(&mut self.open[0]),
            _ => Err(format!(
                "several sessions are open ({}); name one with `session`",
                self.names().join(", ")
            )),
        }
    }

    /// The session `id` names, or the only one when `id` is absent.
    pub fn select(&mut self, id: Option<&str>) -> Result<&mut Session, String> {
        match id {
            Some(id) => self.get(id),
            None => self.only(),
        }
    }

    /// A summary of every open session.
    pub fn summaries(&self) -> Vec<Value> {
        self.open.iter().map(session_summary).collect()
    }

    /// Closes one session, reporting how it ended.
    pub fn close(&mut self, id: &str) -> Result<Value, String> {
        let index = self
            .open
            .iter()
            .position(|session| session.id == id)
            .ok_or_else(|| format!("no session `{id}`"))?;
        let session = self.open.remove(index);
        let (code, errors) = session.close();
        Ok(json!({
            "session": id,
            "closed": true,
            "exit_code": code,
            "adapter_errors": errors,
        }))
    }

    /// Closes every session, for a server that is shutting down.
    pub fn close_all(&mut self) {
        for session in self.open.drain(..) {
            session.close();
        }
    }

    /// The identifiers of the open sessions.
    fn names(&self) -> Vec<String> {
        self.open.iter().map(|session| session.id.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selecting_without_a_name_says_that_nothing_is_open() {
        let mut sessions = Sessions::default();
        let error = sessions.select(None).err().expect("no session is open");
        assert!(error.contains("kira_lldb_launch"), "error was: {error}");
    }

    #[test]
    fn an_unknown_name_is_refused_rather_than_resolved_to_something_else() {
        let mut sessions = Sessions::default();
        let error = sessions.get("s9").err().expect("no session `s9`");
        assert!(error.contains("s9"), "error was: {error}");
        assert!(error.contains("none are open"), "error was: {error}");
    }

    #[test]
    fn closing_an_unknown_session_is_an_error_rather_than_a_silent_success() {
        let mut sessions = Sessions::default();
        assert!(sessions.close("s1").is_err());
    }

    #[test]
    fn an_empty_registry_summarises_to_nothing() {
        assert!(Sessions::default().summaries().is_empty());
    }
}
