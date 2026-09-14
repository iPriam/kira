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
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

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

/// How often the surface repaints itself between phases.
///
/// A redraw is only ever cheaper than the phase it interrupts: one title row
/// and a handful of history rows. This is what keeps the elapsed timer live
/// while a single phase (macro expansion, analysis) holds the build for
/// seconds without reporting anything in between. Fast enough that the
/// shimmer glides instead of stepping.
const TICK: Duration = Duration::from_millis(50);

/// How many cells wide the bright band sweeping the title is: a short comet
/// rather than a wash over the whole line.
const SHIMMER_WIDTH: usize = 6;

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
    /// Repaint counter. Advanced by the ticker as well as by phases, so the
    /// shimmer keeps sweeping even while the build says nothing new.
    frame: u64,
    /// Set while something else owns the terminal (see [`ProgressSink::suspend`]).
    /// The ticker skips repaints until the next phase clears it, so a diagnostic
    /// printed mid-build is not immediately painted over.
    suspended: bool,
    /// Set by [`Surface::finish`]. Tells the ticker to exit on its next wake.
    done: bool,
    /// Whether ANSI styling is wanted on this stderr. Decided once at install
    /// from the same rules as [`kira_toolchain::Paint`]: off for `NO_COLOR`
    /// and for a terminal that declares itself `dumb`.
    styled: bool,
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
        let no_color = std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty());
        let dumb = std::env::var_os("TERM").is_some_and(|term| term == "dumb");
        let surface = Arc::new(Self {
            state: Mutex::new(State {
                title: format!("{command} Kira project"),
                started: Instant::now(),
                history: Vec::new(),
                drawn: 0,
                frame: 0,
                suspended: false,
                done: false,
                styled: !no_color && !dumb,
            }),
        });
        kira_diagnostics::progress::install(surface.clone());
        // Paint once up front, so the title and its timer are on screen from
        // 0.0s rather than only once the first phase arrives.
        if let Ok(mut state) = surface.state.lock() {
            Surface::draw(&mut state);
        }
        spawn_ticker(&surface);
        Some(surface)
    }

    /// Erases the surface and stops receiving phases.
    ///
    /// The surface is scratch: what a build has to say is on stdout, and
    /// leaving a half-drawn status above it would compete with that.
    pub fn finish(&self) {
        kira_diagnostics::progress::uninstall();
        if let Ok(mut state) = self.state.lock() {
            state.done = true;
            erase(&mut state);
        }
    }

    /// Redraws the surface from `state`.
    ///
    /// Styling is applied *after* [`clamp`], so the ANSI escapes never count
    /// toward the drawn width: they are zero-width on screen, and counting
    /// them would wrap a styled row into two physical rows and break the
    /// redraw math.
    fn draw(state: &mut State) {
        let mut out = std::io::stderr().lock();
        let mut buffer = String::new();
        // Back up over what was drawn last time, so the surface stays put
        // instead of scrolling.
        for _ in 0..state.drawn {
            buffer.push_str("\x1b[1A\x1b[2K");
        }
        let elapsed = state.started.elapsed().as_secs_f32();
        // The timer stays plain: the shimmer sweeps the words only, so the
        // numbers never flicker between shades and stay readable at a glance.
        let timer = format!(" ({elapsed:.1}s)");
        let room = WIDTH.saturating_sub(timer.chars().count());
        let name: String = state.title.chars().take(room).collect();
        if state.styled {
            buffer.push_str(&shimmer(&name, state.frame));
        } else {
            buffer.push_str(&name);
        }
        buffer.push_str(&timer);
        buffer.push('\n');
        for line in state.history.iter() {
            buffer.push_str(&clamp(&format!("  {line}")));
            buffer.push('\n');
        }
        state.drawn = state.history.len() + 1;
        let _ = out.write_all(buffer.as_bytes());
        let _ = out.flush();
    }
}

/// Repaints the surface on a timer until it is finished or dropped.
///
/// Progress reporting is event-driven — [`Surface::draw`] runs per phase — and
/// the long phases (`analyzing`, `expanding macros`) hold the build for
/// seconds without emitting one. Without this the elapsed timer visibly
/// stalls and only jumps when the next phase lands. The ticker holds only a
/// [`Weak`] handle, so it exits on its own once the last [`Surface`] is gone;
/// progress stays best-effort and a failed spawn is silently no ticker rather
/// than a failed build.
fn spawn_ticker(surface: &Arc<Surface>) {
    let weak: Weak<Surface> = Arc::downgrade(surface);
    let _ = std::thread::Builder::new()
        .name("kira-progress".to_owned())
        .spawn(move || {
            loop {
                std::thread::sleep(TICK);
                let Some(surface) = weak.upgrade() else {
                    return;
                };
                let Ok(mut state) = surface.state.lock() else {
                    continue;
                };
                if state.done {
                    return;
                }
                if state.suspended || state.drawn == 0 {
                    continue;
                }
                state.frame = state.frame.wrapping_add(1);
                Surface::draw(&mut state);
            }
        });
}

/// Sweeps a whitish band across `line`, whose head sits at `frame`.
///
/// The head travels left to right and on past the end, so the tail fades out
/// off the edge instead of being cut there; then it wraps with a single dark
/// frame between passes rather than a long pause. Each
/// cell behind the head steps one shade down the 256-colour grey ramp
/// (white at the head, fading cell by cell into the plain text), which is
/// what makes the falloff a gradient rather than a hard edge. `line` must
/// already be [`clamp`]ed — escapes are zero-width, so styling first and
/// clamping second would over-count and wrap.
fn shimmer(line: &str, frame: u64) -> String {
    let chars: Vec<char> = line.chars().collect();
    if chars.is_empty() {
        return String::new();
    }
    let head = (frame as usize) % (chars.len() + SHIMMER_WIDTH);
    let mut out = String::new();
    for (index, cell) in chars.iter().enumerate() {
        let behind = head.saturating_sub(index);
        let Some(code) = (index <= head && behind < SHIMMER_WIDTH).then(|| {
            // One grey step per cell back from the head: 255 at the front,
            // fading down the ramp behind it.
            let shade = 255u8.saturating_sub(behind as u8);
            if behind == 0 {
                format!("1;38;5;{shade}")
            } else {
                format!("38;5;{shade}")
            }
        }) else {
            out.push(*cell);
            continue;
        };
        out.push_str("\x1b[");
        out.push_str(&code);
        out.push('m');
        out.push(*cell);
        out.push_str("\x1b[0m");
    }
    out
}

impl ProgressSink for Surface {
    fn suspend(&self) {
        if let Ok(mut state) = self.state.lock() {
            // Flagged before erasing under the same lock, so the ticker cannot
            // slip a repaint in between and paint over the output that asked
            // for the terminal.
            state.suspended = true;
            erase(&mut state);
        }
    }

    fn phase(&self, phase: &str) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        // A new phase means the build is talking again, which ends whatever
        // suspension stood the surface aside.
        state.suspended = false;
        state.history.push(phase.to_owned());
        if state.history.len() > VISIBLE {
            state.history.remove(0);
        }
        Surface::draw(&mut state);
    }

    fn update(&self, phase: &str) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        // A counter refreshes the current line rather than adding one, so a
        // hundred-item run reads as one live line instead of a hundred dead
        // ones scrolling the recent history away.
        state.suspended = false;
        match state.history.last_mut() {
            Some(current) => *current = phase.to_owned(),
            None => state.history.push(phase.to_owned()),
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

    #[test]
    fn a_live_update_refreshes_the_current_line_instead_of_adding_one() {
        let surface = Surface {
            state: Mutex::new(State {
                title: "Testing".to_owned(),
                started: Instant::now(),
                history: vec!["compiling shaders".to_owned()],
                drawn: 0,
                frame: 0,
                suspended: false,
                done: false,
                styled: false,
            }),
        };
        surface.update("compiling shaders (1/2) Glass.ksl");
        surface.update("compiling shaders (2/2) Water.ksl");
        let state = surface.state.lock().expect("the surface");
        assert_eq!(
            state.history,
            vec!["compiling shaders (2/2) Water.ksl".to_owned()]
        );
    }

    /// Strips the `ESC[…m` sequences [`shimmer`] emits, leaving visible text.
    fn visible(text: &str) -> String {
        let mut out = String::new();
        let mut rest = text;
        while let Some(start) = rest.find("\x1b[") {
            out.push_str(&rest[..start]);
            let tail = &rest[start + 2..];
            match tail.find('m') {
                Some(end) => rest = &tail[end + 1..],
                None => {
                    rest = "";
                }
            }
        }
        out.push_str(rest);
        out
    }

    #[test]
    fn the_shimmer_keeps_its_visible_text_and_moves_with_the_frame() {
        let line = "Linting Kira project (1.2s)";
        let first = shimmer(line, 0);
        assert_eq!(visible(&first), line);
        // The band travels: a frame with the head off the end (tail fading
        // out) styles different cells than the opening frame.
        let later = shimmer(line, line.chars().count() as u64);
        assert_eq!(visible(&later), line);
        assert_ne!(first, later);
    }

    #[test]
    fn the_shimmer_on_empty_text_is_empty() {
        assert_eq!(shimmer("", 42), "");
    }
}
