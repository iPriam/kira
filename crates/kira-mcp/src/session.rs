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
/// Ordered by the stamp inside each identifier, not by the identifier. A name
/// begins with its *kind*, so sorting by name sorts alphabetically by kind
/// first: a run saved a second ago as `build-…` would rank below one saved
/// yesterday as `validate-…` and be the one deleted — which is a caller handed
/// an identifier that no longer resolves, immediately, for no reason it could
/// see.
///
/// A file this server did not issue is left alone rather than counted or
/// deleted. The directory is shared with whatever else uses the system
/// temporary directory, and pruning is not a licence to remove someone else's
/// file.
fn prune() {
    let Ok(entries) = std::fs::read_dir(directory()) else {
        return;
    };
    let mut saved: Vec<(u128, u64, PathBuf)> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .filter_map(|path| {
            let (stamp, ordinal) = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .and_then(issued_at)?;
            Some((stamp, ordinal, path))
        })
        .collect();
    if saved.len() <= KEEP {
        return;
    }
    saved.sort_by_key(|(stamp, ordinal, _)| (*stamp, *ordinal));
    for (_, _, path) in &saved[..saved.len() - KEEP] {
        let _ = std::fs::remove_file(path);
    }
}

/// When an identifier this server issued was issued: its stamp and its ordinal.
///
/// `None` for anything else, which is what keeps a foreign file out of the
/// pruning entirely. Read from the right because a kind is free to contain a
/// dash and the two trailing fields never do.
fn issued_at(stem: &str) -> Option<(u128, u64)> {
    let (head, ordinal) = stem.rsplit_once('-')?;
    let (_kind, stamp) = head.rsplit_once('-')?;
    Some((stamp.parse().ok()?, ordinal.parse().ok()?))
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

    /// Age is the stamp, not the name. A `build-` run saved now is newer than a
    /// `validate-` run saved yesterday, and sorting by identifier says the
    /// opposite — which is how a just-saved run came to be the one pruned.
    #[test]
    fn a_runs_age_is_its_stamp_and_not_its_name() {
        let fresh = "build-1786041435682-0000";
        let stale = "validate-1786000000000-0000";
        assert!(
            fresh < stale,
            "by name the fresh run sorts first, which is what made it the one pruned"
        );
        assert!(
            issued_at(fresh) > issued_at(stale),
            "a newer run must outrank an older one whatever its kind is called"
        );
        // Two runs in the same millisecond are ordered by the ordinal that
        // separates them, so pruning never has to pick between them arbitrarily.
        assert!(issued_at("test-1786041435682-0001") > issued_at("test-1786041435682-0000"));
    }

    /// A file this server did not write is not this server's to delete.
    #[test]
    fn a_foreign_file_is_not_a_saved_run() {
        for stem in ["notes", "test-nonsense-0000", "test-1786041435682-x"] {
            assert_eq!(
                issued_at(stem),
                None,
                "`{stem}` is not an issued identifier"
            );
        }
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
