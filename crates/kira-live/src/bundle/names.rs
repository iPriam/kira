//! What makes a payload name safe to put on a disk.
//!
//! A bundle's payload names become file paths under the runner's cache, and a
//! bundle arrives over a socket — so these rules are the only thing between a
//! hostile manifest and a write outside the bundle directory. They live in one
//! place, checked once at the decoder and once at the builder, rather than at
//! each site that later joins a path: a check that has to be remembered is a
//! check that gets forgotten.
//!
//! Every rule applies on every host, never `cfg`'d to the platform it protects.
//! A bundle is built on one machine and staged on another, so a name that is
//! harmless where it was written and an escape where it lands must be rejected
//! by both ends. Deciding by `cfg` would mean the builder cheerfully producing
//! bundles that only the *other* platform's runner refuses.

/// The device names Windows reserves, before any extension.
const RESERVED_DEVICE_NAMES: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Whether `name` is a plain file name — no separators, no traversal, not empty.
///
/// The colon is the subtle one. `"C:evil.dll"` holds no separator, so a
/// separator check alone lets it through; but it is a drive-relative path, and
/// [`Path::join`](std::path::Path::join) replaces the base entirely when what it
/// is given carries a prefix — so `payloads/`.join(`"C:evil.dll"`) is just
/// `C:evil.dll`, written wherever drive C happens to point. A colon also spells
/// an NTFS alternate data stream (`name:stream`), which hides bytes behind a
/// legitimate-looking payload.
pub(crate) fn is_plain_file_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains('\0')
        && !name.contains(':')
        && !is_reserved_device_name(name)
}

/// Whether `name` is a Windows reserved device name.
///
/// Opening one of these on Windows talks to a device rather than creating a
/// file, whatever directory the path names — so a `CON` or `LPT1` payload is not
/// a file a runner can stage, and an extension does not save it: `CON.txt` is
/// still the console.
fn is_reserved_device_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name);
    RESERVED_DEVICE_NAMES
        .iter()
        .any(|reserved| stem.eq_ignore_ascii_case(reserved))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A payload name becomes a path under the bundle directory, so a name that
    /// escapes it is rejected. A bundle is attacker-reachable: it arrives over a
    /// socket.
    #[test]
    fn names_that_escape_are_rejected() {
        for name in [
            "../escape",
            "..",
            ".",
            "",
            "sub/dir",
            "windows\\path",
            "bad\0byte",
            // Drive-relative: no separator, but `Path::join` would drop the
            // bundle directory and write it wherever drive C points.
            "C:evil.dll",
            "c:evil.dll",
            // An NTFS alternate data stream hanging off a legitimate name.
            "app.kbc:hidden",
            // Device names: opening these talks to a device, not a file, and an
            // extension does not make them ordinary.
            "CON",
            "con",
            "NUL",
            "LPT1",
            "COM9",
            "aux.txt",
            "PRN.kbc",
        ] {
            assert!(
                !is_plain_file_name(name),
                "`{name}` must not be a plain file name"
            );
        }
    }

    /// The rules must not swallow ordinary names. A device-name check that
    /// rejected `console.kbc` for starting with `con` would break real bundles,
    /// which is how an over-eager check gets reverted wholesale.
    #[test]
    fn ordinary_names_are_accepted() {
        for name in [
            "app.kbc",
            "libapp.dylib",
            "app.dll",
            "console.kbc",
            "auxiliary.png",
            "communication.kbc",
            "printer.asset",
            "nulled.kbc",
            "a",
            "..leading-dots.kbc",
            "UPPER.KBC",
            "with spaces.kbc",
            "unicode-ünïcode.kbc",
        ] {
            assert!(is_plain_file_name(name), "`{name}` must be accepted");
        }
    }

    /// Every reserved device name is caught, in any case, with or without an
    /// extension — spelled out rather than trusted to the loop above.
    #[test]
    fn every_reserved_device_name_is_caught() {
        for reserved in RESERVED_DEVICE_NAMES {
            assert!(is_reserved_device_name(reserved));
            assert!(is_reserved_device_name(&reserved.to_lowercase()));
            assert!(is_reserved_device_name(&format!("{reserved}.kbc")));
        }
    }
}
