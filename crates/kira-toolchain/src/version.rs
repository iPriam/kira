//! The version every Kira binary reports.
//!
//! Kira versions are `<year>.<month>.<week>[.<increment>]`, and the optional
//! fourth component is not expressible as a Cargo version: `version =
//! "1.8.0.1"` is rejected as `unexpected character '.' after patch version
//! number`. `CARGO_PKG_VERSION` therefore carries only the three-component
//! prefix, and the release job passes the full string through
//! `KIRA_RELEASE_VERSION` so a binary reports the release it belongs to
//! rather than the prefix it was compiled under.
//!
//! A development build sets nothing and falls back to the Cargo version,
//! which is the same string whenever a release carries no increment.

/// The version this binary reports, including any release increment.
pub const RELEASE_VERSION: &str = match option_env!("KIRA_RELEASE_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_version_is_never_empty() {
        assert!(!RELEASE_VERSION.is_empty());
    }

    #[test]
    fn the_version_starts_with_the_cargo_prefix() {
        // Whatever the release job passes in, it must be the Cargo version
        // optionally followed by an increment — never an unrelated string.
        let cargo = env!("CARGO_PKG_VERSION");
        assert!(
            RELEASE_VERSION == cargo
                || RELEASE_VERSION
                    .strip_prefix(cargo)
                    .is_some_and(|rest| rest.starts_with('.')),
            "{RELEASE_VERSION} does not extend {cargo}"
        );
    }
}
