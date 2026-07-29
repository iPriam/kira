//! Where the native half lives, and how a consumer finds it at load time.
//!
//! The VM engine's artifact is data: `include_bytes!` puts the whole library
//! inside the consumer's binary and there is nothing to find. The native engine
//! links a static archive, so the artifact is *in* the binary too, resolved by
//! the linker before the program ever runs. The hybrid engine is the only one of
//! the three with a file that must still be there at run time — its native half
//! is a shared library the process `dlopen`s — and that is a deployment question
//! rather than a build one.
//!
//! # The override, and then the search
//!
//! **`KIRA_<LIBRARY>_NATIVE`**, if set, names the file and is the whole answer:
//! the search below does not run, and a path that is not there fails the load
//! naming it. An operator who says which library they mean gets that one or an
//! error, never a different one — see [`candidates`].
//!
//! Otherwise, two places, in this order, first hit wins:
//!
//! 1. **Beside the consumer's own executable.** The shipping layout: copy
//!    `lib<name>.dylib` next to the binary and the program is deployable. This
//!    is the entry that makes a hybrid library a thing you can hand to someone,
//!    and it is checked before the build path so that a deployed copy is never
//!    silently passed over in favour of a developer's build directory that
//!    happens to still exist on the same machine.
//! 2. **The absolute path the build baked in.** The development layout: `cargo
//!    test` right after `kira build --backend hybrid` works with no ceremony,
//!    because the generator wrote down where it put the file. Last, because it
//!    is the one that is wrong the moment the artifact is copied anywhere.
//!
//! # Why a missing file is a typed error naming every path tried
//!
//! "Library not found" is the least useful thing a loader can say, and the
//! answer a user needs is which places were looked in — the directory they
//! thought they copied into, or the override they thought they set. So
//! [`NativeHalfMissing`](crate::HybridMainError::NativeHalfMissing) carries the
//! whole list, in the order it was tried.

use std::path::{Path, PathBuf};

/// The platform's shared-library file name for a Kira library.
///
/// One spelling, derived here, so the builder that writes the file and the
/// consumer that looks for it can never disagree about what it is called.
pub fn shared_library_file_name(library: &str) -> String {
    format!("{DYLIB_PREFIX}{library}{DYLIB_SUFFIX}")
}

/// The environment variable that overrides where the native half is found.
///
/// `uifoundation` becomes `KIRA_UIFOUNDATION_NATIVE`. Uppercased and with every
/// character that cannot appear in an environment variable name replaced by an
/// underscore, so a library name a shell cannot spell still gets an override.
pub fn override_variable(library: &str) -> String {
    let mut name = String::with_capacity(library.len() + 12);
    name.push_str("KIRA_");
    for ch in library.chars() {
        if ch.is_ascii_alphanumeric() {
            name.extend(ch.to_uppercase());
        } else {
            name.push('_');
        }
    }
    name.push_str("_NATIVE");
    name
}

/// `lib` on every platform this targets; Windows is not one of them yet.
const DYLIB_PREFIX: &str = "lib";

/// The extension a shared library carries on this platform.
const DYLIB_SUFFIX: &str = if cfg!(target_os = "macos") {
    ".dylib"
} else {
    ".so"
};

/// Every path the search would try, in order, for a library of this name.
///
/// Returned as a list rather than as the first hit so that a failure can name
/// all of them and a test can assert the *order* — which is the part of this
/// design that is a decision rather than a mechanism.
///
/// `baked` is the absolute path the generator wrote down at build time, and
/// `override_path` is what `KIRA_<LIBRARY>_NATIVE` was set to, if anything.
///
/// # Why the override arrives as an argument
///
/// So this function reads no environment and touches no global state, which
/// makes the order — the part of this design that is a decision — assertable by
/// an ordinary test. A version that read the variable itself could only be
/// tested by setting it, and setting an environment variable is process-wide:
/// the tests in one binary run on parallel threads, so such a test changes what
/// its neighbours resolve. [`find`] is the one place that reads it.
///
/// # The override does not fall through
///
/// When it is set it is the *only* candidate, rather than the first of three. An
/// override that fell through would mean an operator who pointed at one library
/// and got another — silently, and most often on the one machine where the other
/// entries happen to resolve, which is the machine they were debugging on.
/// Loading something other than what was asked for and saying nothing is the
/// failure this whole search order exists to avoid, so the override is
/// authoritative: set it wrong and the load fails naming it.
pub fn candidates(library: &str, baked: &Path, override_path: Option<&Path>) -> Vec<PathBuf> {
    if let Some(chosen) = override_path {
        return vec![chosen.to_path_buf()];
    }
    let file = shared_library_file_name(library);
    let mut paths = Vec::with_capacity(2);
    if let Ok(executable) = std::env::current_exe()
        && let Some(directory) = executable.parent()
    {
        paths.push(directory.join(&file));
    }
    paths.push(baked.to_path_buf());
    paths
}

/// The first candidate that exists, or every path that was tried.
///
/// The one place `KIRA_<LIBRARY>_NATIVE` is read; [`candidates`] decides what
/// that means.
pub fn find(library: &str, baked: &Path) -> Result<PathBuf, Vec<PathBuf>> {
    let chosen = std::env::var_os(override_variable(library)).map(PathBuf::from);
    let tried = candidates(library, baked, chosen.as_deref());
    match tried.iter().find(|path| path.is_file()) {
        Some(found) => Ok(found.clone()),
        None => Err(tried),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_file_name_is_the_platforms_own_spelling() {
        let name = shared_library_file_name("uifoundation");
        assert!(name.starts_with("libuifoundation."), "{name}");
        if cfg!(target_os = "macos") {
            assert_eq!(name, "libuifoundation.dylib");
        } else {
            assert_eq!(name, "libuifoundation.so");
        }
    }

    #[test]
    fn the_override_variable_is_derived_from_the_library_name() {
        assert_eq!(
            override_variable("uifoundation"),
            "KIRA_UIFOUNDATION_NATIVE"
        );
    }

    #[test]
    fn a_library_name_a_shell_cannot_spell_still_gets_an_override() {
        // Nothing stops a package being called `ui-foundation`, and a variable
        // with a hyphen in it is not settable from most shells. Mapped rather
        // than refused: the override exists to be usable.
        assert_eq!(
            override_variable("ui-foundation"),
            "KIRA_UI_FOUNDATION_NATIVE"
        );
    }

    #[test]
    fn the_baked_build_path_is_tried_last() {
        // The order is the design. A deployed copy beside the executable must
        // win over a build directory that happens to survive on the same
        // machine, or "I copied the dylib next to my binary" would silently do
        // nothing on the developer's own box and work everywhere else.
        let baked = PathBuf::from("/build/lib/libuifoundation.dylib");
        let tried = candidates("uifoundation", &baked, None);
        assert_eq!(tried.last(), Some(&baked), "{tried:?}");
    }

    #[test]
    fn the_executable_directory_is_tried_before_the_build_path() {
        let baked = PathBuf::from("/build/lib/libuifoundation.dylib");
        let tried = candidates("uifoundation", &baked, None);
        // The test binary always has a path, so this entry is always present.
        assert!(tried.len() >= 2, "{tried:?}");
        assert!(
            tried[tried.len() - 2].ends_with(shared_library_file_name("uifoundation")),
            "{tried:?}"
        );
    }

    #[test]
    fn an_override_replaces_the_search_rather_than_leading_it() {
        // The decision this file is really about. An operator who sets the
        // variable has said which file they mean; falling through to another one
        // would hand them a library they did not name, on exactly the machine
        // where the fallbacks still resolve.
        let chosen = PathBuf::from("/elsewhere/libui.dylib");
        let tried = candidates(
            "uifoundation",
            Path::new("/build/libuifoundation.dylib"),
            Some(&chosen),
        );
        assert_eq!(tried, [chosen], "{tried:?}");
    }

    #[test]
    fn a_missing_library_reports_every_path_it_looked_in() {
        let baked = PathBuf::from("/nonexistent/libnothing.dylib");
        let tried = find("nothing", &baked).expect_err("nothing is there");
        assert!(tried.contains(&baked), "{tried:?}");
    }
}
