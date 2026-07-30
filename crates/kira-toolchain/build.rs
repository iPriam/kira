//! Rebuild when the release version changes.
//!
//! `version.rs` bakes `KIRA_RELEASE_VERSION` in through `option_env!`, which
//! cargo cannot see as an input on its own; without this the first build of a
//! checkout would pin the version for every later one.

fn main() {
    println!("cargo::rerun-if-env-changed=KIRA_RELEASE_VERSION");
}
