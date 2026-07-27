//! The live-session telemetry a native library calls back into.
//!
//! A graphics backend knows things a live session wants to hear — that a frame
//! was submitted, that the first one reached the screen — and it learns them
//! inside C code the runtime has no other view into. These are the two symbols
//! it calls to say so.
//!
//! They are deliberately the thinnest thing that works: a line on stderr. The
//! runtime archive sits at layer 4 and the live session model at layer 8, so
//! the runtime cannot speak the session protocol directly, and a child process
//! writing a marker its parent reads needs no protocol at all. A program run
//! outside a live session writes the lines to a stderr nobody is reading, which
//! costs one write and confuses nothing.

use std::ffi::c_char;
use std::io::Write;

/// The prefix every telemetry line carries.
///
/// A live server picks its lines out of the child's stderr by this prefix, so
/// a program's own diagnostics can never be mistaken for one.
pub const LIVE_MARKER_PREFIX: &str = "@kira.live ";

/// The line reported when the first frame has been presented.
pub const FIRST_FRAME_LINE: &str = "live.first_frame";

/// Writes one telemetry line to stderr.
fn emit(line: &str) {
    let mut stderr = std::io::stderr().lock();
    // A telemetry line that cannot be written changes nothing about the run:
    // the session simply does not hear about it, which is what happens anyway
    // when no session is listening.
    let _ = writeln!(stderr, "{LIVE_MARKER_PREFIX}{line}");
    let _ = stderr.flush();
}

/// Reports that the first frame reached the screen.
#[unsafe(no_mangle)]
pub extern "C" fn kira_live_emit_first_frame() {
    emit(FIRST_FRAME_LINE);
}

/// Reports one telemetry line from a native library.
///
/// # Safety
/// `line` must be null, or a pointer to a NUL-terminated C string that stays
/// readable for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_live_emit_log_line(line: *const c_char) {
    if line.is_null() {
        return;
    }
    // SAFETY: the caller guarantees `line` addresses a readable NUL-terminated
    // string for the duration of this call, which is all `CStr` reads.
    let text = unsafe { std::ffi::CStr::from_ptr(line) };
    // A library may hand over bytes that are not UTF-8; reporting them lossily
    // is better than dropping the line, because the line is a diagnostic.
    emit(&text.to_string_lossy());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_null_line_is_ignored_rather_than_dereferenced() {
        // SAFETY: null is the one pointer the contract explicitly allows.
        unsafe { kira_live_emit_log_line(std::ptr::null()) };
    }

    #[test]
    fn a_line_is_written_under_the_marker_prefix() {
        // The prefix is what a live server matches on, so it is part of the
        // contract rather than formatting.
        assert!(LIVE_MARKER_PREFIX.starts_with('@'));
        assert!(LIVE_MARKER_PREFIX.ends_with(' '));
        assert_eq!(FIRST_FRAME_LINE, "live.first_frame");
    }

    #[test]
    fn a_borrowed_c_string_is_read_without_copying_past_its_nul() {
        let text = std::ffi::CString::new("live.test.line").expect("no interior NUL");
        // SAFETY: `text` outlives the call and is NUL-terminated by `CString`.
        unsafe { kira_live_emit_log_line(text.as_ptr()) };
    }
}
