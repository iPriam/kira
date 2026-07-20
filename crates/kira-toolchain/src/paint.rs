//! Terminal styling with no dependency: a handful of ANSI wrappers behind one
//! switch.
//!
//! Color is a property of *where the text is going*, so the switch is decided
//! once from the stream and the environment ([`Paint::auto`]) and then carried
//! by value; everything downstream styles unconditionally and stays testable
//! by constructing [`Paint::plain`]. Honors the `NO_COLOR` convention and a
//! `TERM=dumb` terminal.

use std::io::IsTerminal as _;

/// The styling switch: wraps text in ANSI codes, or returns it untouched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Paint {
    enabled: bool,
}

impl Paint {
    /// Styling decided by stdout: on for an interactive terminal, off for a
    /// pipe, a file, `NO_COLOR`, or a terminal that declares itself `dumb`.
    #[must_use]
    pub fn auto() -> Self {
        let no_color = std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty());
        let dumb = std::env::var_os("TERM").is_some_and(|term| term == "dumb");
        Self {
            enabled: std::io::stdout().is_terminal() && !no_color && !dumb,
        }
    }

    /// Styling decided by stderr, for text printed there — usage on a usage
    /// error, diagnostics. Same rules as [`Paint::auto`], different stream:
    /// stdout being piped says nothing about where stderr is going.
    #[must_use]
    pub fn auto_stderr() -> Self {
        let no_color = std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty());
        let dumb = std::env::var_os("TERM").is_some_and(|term| term == "dumb");
        Self {
            enabled: std::io::stderr().is_terminal() && !no_color && !dumb,
        }
    }

    /// Styling forced off: what every non-terminal consumer gets.
    #[must_use]
    pub fn plain() -> Self {
        Self { enabled: false }
    }

    /// Bold, for headers and the words a scanning eye should land on.
    #[must_use]
    pub fn bold(self, text: &str) -> String {
        self.wrap("1", text)
    }

    /// Dim, for the explanatory text beside a command.
    #[must_use]
    pub fn dim(self, text: &str) -> String {
        self.wrap("2", text)
    }

    /// Green, for the selected toolchain and other good news.
    #[must_use]
    pub fn green(self, text: &str) -> String {
        self.wrap("32", text)
    }

    /// Yellow, for a warning that is not a failure.
    #[must_use]
    pub fn yellow(self, text: &str) -> String {
        self.wrap("33", text)
    }

    /// Cyan, for the commands themselves.
    #[must_use]
    pub fn cyan(self, text: &str) -> String {
        self.wrap("36", text)
    }

    fn wrap(self, code: &str, text: &str) -> String {
        if self.enabled {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_never_emits_escape_codes() {
        assert_eq!(Paint::plain().bold("x"), "x");
        assert_eq!(Paint::plain().green("x"), "x");
    }

    #[test]
    fn enabled_wraps_and_resets() {
        let paint = Paint { enabled: true };
        assert_eq!(paint.cyan("knvm"), "\x1b[36mknvm\x1b[0m");
    }
}
