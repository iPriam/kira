//! The status surface a build draws while it works.
//!
//! A title line and the last few phases, redrawn in place on stderr. Only when
//! stderr is a terminal: piped output belongs to whatever is reading it, and a
//! log full of cursor-movement escapes helps nobody.
//!
//! # Why stderr, and why in place
//!
//! stdout carries the build's *result* — the artifact path, the diagnostics a
//! tool parses — and progress is not part of that. Drawing in place keeps the
//! surface to a fixed height, so a two-minute build scrolls nothing away and
//! what is left on screen at the end is what the build actually said.

use std::io::{IsTerminal, Write};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use kira_diagnostics::progress::ProgressSink;

/// Prints to stdout with the status surface stood aside first.
///
/// The surface erases itself by moving the cursor up over the rows it drew.
/// Anything printed between the last redraw and that erase moves the cursor
/// too, so the erase walks up from the wrong place and wipes the *output*
/// instead of the surface — which is how a command can fail and leave nothing
/// but a stale title line on screen.
///
/// Suspending first is the fix, and going through these macros is what makes
/// it hold: a bare `println!` added later is the bug coming back. Suspending
/// twice is free — the erase does nothing once the surface is already down.
macro_rules! out {
    ($($argument:tt)*) => {{
        let _suspended = kira_diagnostics::progress::suspended();
        println!($($argument)*);
    }};
}

/// [`out`] for stderr: diagnostics, refusals, and every `kira: …` failure.
macro_rules! err {
    ($($argument:tt)*) => {{
        let _suspended = kira_diagnostics::progress::suspended();
        eprintln!($($argument)*);
    }};
}

pub(crate) use {err, out};

/// How many recent phases stay on screen.
const VISIBLE: usize = 6;

/// The widest line drawn, short enough that an 80-column terminal never wraps
/// one status row into two physical rows and breaks the redraw.
const WIDTH: usize = 72;

/// A drawn status surface.
pub struct Surface {
    state: Mutex<State>,
}

/// Everything the surface redraws from.
struct State {
    title: String,
    started: Instant,
    history: Vec<String>,
    drawn: usize,
}

impl Surface {
    /// Installs a surface for `command`, when stderr is a terminal.
    ///
    /// Returns `None` when it is not, and installs nothing — a piped build
    /// stays exactly as quiet as it was.
    pub fn install(command: &str) -> Option<Arc<Self>> {
        if !std::io::stderr().is_terminal() {
            return None;
        }
        let surface = Arc::new(Self {
            state: Mutex::new(State {
                title: format!("{command} Kira project"),
                started: Instant::now(),
                history: Vec::new(),
                drawn: 0,
            }),
        });
        kira_diagnostics::progress::install(surface.clone());
        Some(surface)
    }

    /// Erases the surface and stops receiving phases.
    ///
    /// The surface is scratch: what a build has to say is on stdout, and
    /// leaving a half-drawn status above it would compete with that.
    pub fn finish(&self) {
        kira_diagnostics::progress::uninstall();
        if let Ok(mut state) = self.state.lock() {
            erase(&mut state);
        }
    }

    /// Redraws the surface from `state`.
    fn draw(state: &mut State) {
        let mut out = std::io::stderr().lock();
        let mut buffer = String::new();
        // Back up over what was drawn last time, so the surface stays put
        // instead of scrolling.
        for _ in 0..state.drawn {
            buffer.push_str("\x1b[1A\x1b[2K");
        }
        let elapsed = state.started.elapsed().as_secs_f32();
        buffer.push_str(&clamp(&format!("{} ({elapsed:.1}s)", state.title)));
        buffer.push('\n');
        for line in &state.history {
            buffer.push_str(&clamp(&format!("  {line}")));
            buffer.push('\n');
        }
        state.drawn = state.history.len() + 1;
        let _ = out.write_all(buffer.as_bytes());
        let _ = out.flush();
    }
}

impl ProgressSink for Surface {
    fn suspend(&self) {
        if let Ok(mut state) = self.state.lock() {
            erase(&mut state);
        }
    }

    fn phase(&self, phase: &str) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        state.history.push(phase.to_owned());
        if state.history.len() > VISIBLE {
            state.history.remove(0);
        }
        Surface::draw(&mut state);
    }
}

/// Erases every drawn row.
fn erase(state: &mut State) {
    if state.drawn == 0 {
        return;
    }
    let mut out = std::io::stderr().lock();
    let mut buffer = String::new();
    for _ in 0..state.drawn {
        buffer.push_str("\x1b[1A\x1b[2K");
    }
    state.drawn = 0;
    let _ = out.write_all(buffer.as_bytes());
    let _ = out.flush();
}

/// Truncates `line` to the drawn width, on a character boundary.
fn clamp(line: &str) -> String {
    if line.chars().count() <= WIDTH {
        return line.to_owned();
    }
    // One character short of the width leaves room for the ellipsis, and
    // counting characters rather than bytes keeps a multi-byte name whole.
    let kept: String = line.chars().take(WIDTH - 1).collect();
    format!("{kept}…")
}

/// Takes the surface down when the command returns, however it returns.
///
/// A command has many exits — every early `return` on a bad option or a failed
/// analysis — and a surface left installed would draw over whatever came next.
pub struct Finish(pub Option<Arc<Surface>>);

impl Drop for Finish {
    fn drop(&mut self) {
        if let Some(surface) = &self.0 {
            surface.finish();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_within_the_width_is_left_alone() {
        assert_eq!(clamp("parsing"), "parsing");
    }

    #[test]
    fn a_long_line_is_truncated_on_a_character_boundary() {
        let long = "é".repeat(WIDTH * 2);
        let clamped = clamp(&long);
        assert_eq!(clamped.chars().count(), WIDTH);
        assert!(clamped.ends_with('…'));
        // The point of counting characters: a byte-wise cut would split one of
        // these in half and produce something that is not text.
        assert!(std::str::from_utf8(clamped.as_bytes()).is_ok());
    }

    #[test]
    fn nothing_is_installed_when_stderr_is_not_a_terminal() {
        // Under a test harness stderr is captured, never a terminal, so this
        // also pins that a piped build stays silent.
        if !std::io::stderr().is_terminal() {
            assert!(Surface::install("Building").is_none());
            assert!(!kira_diagnostics::progress::listening());
        }
    }
}
