//! The platform libraries a Kira native artifact links against.
//!
//! # One list, three consumers
//!
//! A Kira archive carries a Rust `staticlib`, and a Rust `staticlib` bundles the
//! standard library but not the platform libraries it calls into. Three places
//! need to name those: this crate's linker driver when it links an executable,
//! the `build.rs` the wrapper generator writes into a consumer's crate, and a
//! consumer that reaches the generated wrapper another way. Each of them held
//! its own hand-written copy, which is the same second-derivation shape the
//! `kira_lib_*` symbols are deliberately kept away from — a library added here
//! would leave the other two behind, and the failure would surface as an
//! undefined symbol in somebody else's link, naming nothing.
//!
//! So the list lives here as plain data, once. This module is deliberately *not*
//! behind the `llvm` feature: the names are facts about a host, not something
//! LLVM answers, and a consumer's build script must reach them on a machine with
//! no LLVM at all.
//!
//! The contents are what `rustc --print native-static-libs` reports for each
//! host, minus the ones a clang driver already links by default (`-lSystem`,
//! `-lc`).

/// The system libraries and frameworks one host needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlatformLinkList {
    /// The `target_os` this list belongs to, spelled as `cfg(target_os = ...)`
    /// spells it.
    pub target_os: &'static str,
    /// Libraries, without the `-l`.
    pub libraries: &'static [&'static str],
    /// Frameworks, which only Apple platforms have.
    pub frameworks: &'static [&'static str],
}

/// Every host this backend knows how to link for.
///
/// A host that is not here links against nothing extra, which is what an
/// unrecognised platform got before this list existed.
pub const PLATFORM_LINK_LISTS: &[PlatformLinkList] = &[
    PlatformLinkList {
        // Rust's std on Apple platforms resolves names and unwinds through
        // these; the driver supplies libSystem itself.
        target_os: "macos",
        libraries: &["resolv", "c++"],
        frameworks: &["CoreFoundation"],
    },
    PlatformLinkList {
        target_os: "linux",
        libraries: &["pthread", "dl", "m", "rt", "gcc_s", "util"],
        frameworks: &[],
    },
    PlatformLinkList {
        // Windows was absent, so `link_list_for` answered with the empty list
        // and a hybrid link went out carrying a Rust `staticlib` and none of
        // the import libraries its standard library calls into — 33 unresolved
        // `__imp_*` externals, every one of them explained by a name below:
        // the sockets (`accept`, `bind`, `getaddrinfo`) are ws2_32, the home
        // directory (`GetUserProfileDirectoryW`) is userenv, and the file
        // opening (`NtCreateFile`) is ntdll.
        //
        // The CRT itself is not here: the driver links it, exactly as
        // `-lSystem` and `-lc` are left off the two lists above.
        target_os: "windows",
        libraries: &[
            "kernel32",
            "advapi32",
            "bcrypt",
            "ntdll",
            "userenv",
            "ws2_32",
            "dbghelp",
            "synchronization",
            "ole32",
            "oleaut32",
            "shell32",
            "uuid",
        ],
        frameworks: &[],
    },
];

/// The empty list, for a host nothing above names.
const NOTHING: PlatformLinkList = PlatformLinkList {
    target_os: "",
    libraries: &[],
    frameworks: &[],
};

/// The list for `target_os`, or an empty one when it is not a host we know.
pub fn link_list_for(target_os: &str) -> &'static PlatformLinkList {
    PLATFORM_LINK_LISTS
        .iter()
        .find(|list| list.target_os == target_os)
        .unwrap_or(&NOTHING)
}

/// The list for the host this crate was compiled for.
pub fn host_link_list() -> &'static PlatformLinkList {
    link_list_for(std::env::consts::OS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_host_this_test_runs_on_is_one_the_backend_knows() {
        // Not a tautology: it fails on a host the backend would silently link
        // nothing extra for, which is a link failure waiting to happen in a
        // consumer's crate rather than here.
        let list = host_link_list();
        assert_eq!(list.target_os, std::env::consts::OS, "{list:?}");
    }

    /// Every platform the gate builds on names its libraries here.
    ///
    /// The empty list is the answer for a host nobody taught this module about,
    /// and it is indistinguishable from "this host needs nothing" right up
    /// until the link fails in somebody else's crate naming nothing. Windows
    /// sat in that gap: supported by the toolchain, absent from this table, and
    /// silently linked against no import libraries at all. This is what makes
    /// adding a platform to CI without adding it here a failing test rather
    /// than a puzzling one.
    #[test]
    fn every_host_the_gate_builds_on_names_its_libraries() {
        for os in ["macos", "linux", "windows"] {
            let list = link_list_for(os);
            assert_eq!(list.target_os, os, "`{os}` is not in the table");
            assert!(
                !list.libraries.is_empty(),
                "`{os}` links against nothing extra, which is a link failure \
                 waiting to happen in a consumer's crate"
            );
        }
    }

    #[test]
    fn an_unknown_host_asks_for_nothing_rather_than_guessing() {
        let list = link_list_for("solaris");
        assert!(list.libraries.is_empty() && list.frameworks.is_empty());
    }

    #[test]
    fn only_apple_platforms_have_frameworks() {
        for list in PLATFORM_LINK_LISTS {
            if list.target_os != "macos" {
                assert!(list.frameworks.is_empty(), "{list:?}");
            }
        }
    }
}
