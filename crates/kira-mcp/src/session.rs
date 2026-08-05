//! Saved runs, so a failure can be read twice without being produced twice.
//!
//! A test run that reports "seven failed" and nothing else forces the caller to
//! run the whole suite again to see what failed. Every run is written here whole
//! and answered with an identifier, so the summary can stay small while the
//! detail stays one lookup away.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

/// How many saved runs are kept before the oldest are removed.
const KEEP: usize = 32;

/// Distinguishes two runs saved in the same millisecond.
static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Why a saved run could not be written or read.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// No run is saved under that identifier.
    #[error("no saved run has the identifier `{0}`")]
    Unknown(String),
    /// The identifier is not one this server issues.
    ///
    /// Checked before the identifier reaches a path: an id is a filename, and a
    /// caller that passed `../../something` would otherwise be reading a file of
    /// their choosing through a tool that only promises to read its own runs.
    #[error("`{0}` is not a well-formed run identifier")]
    Malformed(String),
    #[error("cannot access the saved run `{id}`: {source}")]
    Io {
        id: String,
        #[source]
        source: std::io::Error,
    },
    #[error("the saved run `{id}` is not readable as a result: {source}")]
    Corrupt {
        id: String,
        #[source]
        source: serde_json::Error,
    },
}

/// Where saved runs live.
///
/// The system temporary directory, not the repository: these are byproducts of
/// answering a question, and a tool that grew the working tree every time it ran
/// the tests would be a tool nobody leaves enabled.
pub fn directory() -> PathBuf {
    std::env::temp_dir().join("kira-mcp-runs")
}

/// Saves `value` and returns the identifier it was saved under.
pub fn store(kind: &str, value: &Value) -> Result<String, SessionError> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or_default();
    let ordinal = COUNTER.fetch_add(1, Ordering::Relaxed);
    let id = format!("{kind}-{stamp:013}-{ordinal:04}");

    let directory = directory();
    std::fs::create_dir_all(&directory).map_err(|source| SessionError::Io {
        id: id.clone(),
        source,
    })?;
    let rendered = serde_json::to_string(value).map_err(|source| SessionError::Corrupt {
        id: id.clone(),
        source,
    })?;
    std::fs::write(directory.join(format!("{id}.json")), rendered).map_err(|source| {
        SessionError::Io {
            id: id.clone(),
            source,
        }
    })?;
    prune();
    Ok(id)
}

/// Reads back a saved run.
pub fn load(id: &str) -> Result<Value, SessionError> {
    if !well_formed(id) {
        return Err(SessionError::Malformed(id.to_owned()));
    }
    let path = directory().join(format!("{id}.json"));
    if !path.is_file() {
        return Err(SessionError::Unknown(id.to_owned()));
    }
    let text = std::fs::read_to_string(&path).map_err(|source| SessionError::Io {
        id: id.to_owned(),
        source,
    })?;
    serde_json::from_str(&text).map_err(|source| SessionError::Corrupt {
        id: id.to_owned(),
        source,
    })
}

/// Whether `id` is one this server could have issued.
fn well_formed(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
}

/// Removes all but the most recent [`KEEP`] saved runs.
///
/// Identifiers begin with a fixed-width millisecond stamp, so sorting by name
/// sorts by age.
fn prune() {
    let Ok(entries) = std::fs::read_dir(directory()) else {
        return;
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect();
    if files.len() <= KEEP {
        return;
    }
    files.sort();
    for path in &files[..files.len() - KEEP] {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_saved_run_reads_back_whole() {
        let value = json!({ "success": false, "failures": [{ "kind": "test_failure" }] });
        let id = store("test", &value).expect("the run is saved");
        assert_eq!(load(&id).expect("the run is found"), value);
    }

    /// An identifier that is not one of ours never reaches a path.
    #[test]
    fn a_traversing_identifier_is_refused_before_it_reaches_the_filesystem() {
        for id in ["../secrets", "a/b", "..", "", "with space"] {
            assert!(
                matches!(load(id), Err(SessionError::Malformed(_))),
                "`{id}` must be refused as malformed"
            );
        }
    }

    #[test]
    fn an_unsaved_identifier_is_reported_as_unknown() {
        let error = load("test-0000000000000-9999").expect_err("nothing is saved under it");
        assert!(matches!(error, SessionError::Unknown(_)));
    }

    /// Two runs saved in the same millisecond get different identifiers.
    #[test]
    fn identifiers_do_not_collide() {
        let first = store("test", &json!({ "n": 1 })).expect("saved");
        let second = store("test", &json!({ "n": 2 })).expect("saved");
        assert_ne!(first, second);
        assert_eq!(load(&first).expect("found")["n"], json!(1));
        assert_eq!(load(&second).expect("found")["n"], json!(2));
    }
}
