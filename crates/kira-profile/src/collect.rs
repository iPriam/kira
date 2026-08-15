//! Getting machine samples from the platform's own profiler.
//!
//! Every platform already has a sampling profiler that knows how to unwind its
//! own stacks and read its own symbol format. Kira drives that rather than
//! reimplementing it: `perf` on Linux, Instruments on macOS, and the Windows
//! debugging facility on Windows. What this module owns is the shape they all
//! deliver into — a [`Profile`] of machine frames — so the report a reader sees
//! is the same one on every platform.
//!
//! # One shape: launch and follow
//!
//! A recording always *launches* the program it profiles, never attaches to a
//! process already running. That is what makes the three collectors the same
//! program: `perf record -- cmd` and `xctrace record --launch` are each their
//! platform's ordinary spelling, no elevated permission is needed to profile
//! your own child, and the samples start at the program's first instruction
//! rather than wherever the profiler happened to catch up.
//!
//! For a VM or hybrid run the child is `kira` itself running the program; for a
//! native run it is the built executable. Both are one process to profile.

use std::path::PathBuf;

use crate::model::Profile;
use crate::symbols::KiraSymbols;

#[cfg(target_os = "macos")]
mod instruments;
#[cfg(target_os = "linux")]
mod perf;
#[cfg(all(windows, any(target_arch = "x86_64", target_arch = "aarch64")))]
mod windows;

/// The program a recording starts and profiles.
#[derive(Debug, Clone)]
pub struct Launch {
    /// The executable to start.
    pub program: PathBuf,
    /// Its arguments.
    pub arguments: Vec<String>,
    /// Environment entries to add for the child.
    pub environment: Vec<(String, String)>,
    /// What a report should call this run.
    ///
    /// The executable's own name is wrong whenever the executable is not the
    /// program: a VM run launches `kira`, and a reader of that profile wants
    /// their program's name in the command column, not the compiler's.
    pub label: Option<String>,
}

impl Launch {
    /// A launch of `program` with no arguments and no added environment.
    #[must_use]
    pub fn new(program: PathBuf) -> Self {
        Self {
            program,
            arguments: Vec::new(),
            environment: Vec::new(),
            label: None,
        }
    }

    /// The name a report's command column shows.
    #[must_use]
    pub fn command(&self) -> String {
        if let Some(label) = &self.label {
            return label.clone();
        }
        self.program
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.program.to_string_lossy().into_owned())
    }
}

/// What a recording asks the platform profiler for.
#[derive(Debug, Clone, Copy)]
pub struct CollectOptions {
    /// Samples per second.
    pub frequency: u32,
    /// Whether to record a call graph rather than only the innermost frame.
    pub call_graph: bool,
    /// The deepest stack to record.
    pub max_depth: u32,
}

impl Default for CollectOptions {
    fn default() -> Self {
        Self {
            frequency: DEFAULT_FREQUENCY,
            call_graph: true,
            max_depth: 128,
        }
    }
}

/// The default sampling frequency, in hertz.
///
/// A prime number, for the same reason `perf` defaults to one: a frequency
/// that shares a factor with a program's own periodic work samples that work at
/// the same phase every time and reports a loop that runs at 1 kHz as either
/// all of the profile or none of it.
pub const DEFAULT_FREQUENCY: u32 = 997;

/// Why machine samples could not be collected.
#[derive(Debug, thiserror::Error)]
pub enum CollectError {
    /// This platform has no profiler this build can drive.
    #[error("{tool} cannot profile this run: {reason}")]
    Unavailable {
        /// The tool that was going to be used.
        tool: &'static str,
        /// Why it cannot be.
        reason: String,
    },
    /// The program could not be started.
    #[error("cannot start `{program}`: {source}")]
    Spawn {
        /// The program that would not start.
        program: PathBuf,
        /// The underlying failure.
        #[source]
        source: std::io::Error,
    },
    /// The platform profiler failed.
    #[error("{tool} failed: {problem}")]
    Tool {
        /// The tool that failed.
        tool: &'static str,
        /// What it reported.
        problem: String,
    },
    /// The platform profiler's output could not be understood.
    #[error("{tool} produced output this build cannot read: {problem}")]
    Parse {
        /// The tool whose output was unreadable.
        tool: &'static str,
        /// What was wrong with it.
        problem: String,
    },
    /// An operating-system call failed.
    #[error("{call} failed with code {code}")]
    Platform {
        /// The call that failed.
        call: &'static str,
        /// The code it reported.
        code: u32,
    },
    /// Reading or writing the profiler's own files failed.
    #[error("{action}: {source}")]
    Io {
        /// What was being done.
        action: String,
        /// The underlying failure.
        #[source]
        source: std::io::Error,
    },
}

/// A running recording of one child process.
#[derive(Debug)]
pub struct MachineRecorder {
    inner: Inner,
}

#[derive(Debug)]
enum Inner {
    /// The platform's profiler is driving the child.
    Platform(PlatformRecorder),
    /// No profiler: the child still runs, and the recording has no machine view.
    Plain(plain::Recorder),
}

/// The collector this build drives.
///
/// Windows unwinds with DbgHelp, which this build uses on the architectures
/// whose stacks it can walk; every other supported platform drives its own
/// command-line profiler. A platform with neither still records the Kira view.
#[cfg(target_os = "linux")]
type PlatformRecorder = perf::Recorder;
#[cfg(target_os = "macos")]
type PlatformRecorder = instruments::Recorder;
#[cfg(all(windows, any(target_arch = "x86_64", target_arch = "aarch64")))]
type PlatformRecorder = windows::Recorder;
#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    all(windows, any(target_arch = "x86_64", target_arch = "aarch64"))
)))]
type PlatformRecorder = unsupported::Recorder;

impl MachineRecorder {
    /// The name of the profiler this platform would use.
    #[must_use]
    pub fn tool() -> &'static str {
        PlatformRecorder::TOOL
    }

    /// Starts the program under the platform's profiler.
    pub fn start(launch: &Launch, options: &CollectOptions) -> Result<Self, CollectError> {
        Ok(Self {
            inner: Inner::Platform(PlatformRecorder::start(launch, options)?),
        })
    }

    /// Starts the program with no profiler attached.
    ///
    /// What a recording falls back to when the platform's profiler is not
    /// installed: the run still happens and a VM or hybrid run still produces
    /// its Kira view, so a machine that cannot profile machine frames is not a
    /// machine that cannot profile Kira.
    pub fn start_unprofiled(launch: &Launch) -> Result<Self, CollectError> {
        Ok(Self {
            inner: Inner::Plain(plain::Recorder::start(launch)?),
        })
    }

    /// Waits for the program to finish, returning its exit code.
    pub fn wait(&mut self) -> Result<i32, CollectError> {
        match &mut self.inner {
            Inner::Platform(recorder) => recorder.wait(),
            Inner::Plain(recorder) => recorder.wait(),
        }
    }

    /// Stops the profiler and builds the machine view.
    pub fn finish(self, symbols: &KiraSymbols) -> Result<Profile, CollectError> {
        match self.inner {
            Inner::Platform(recorder) => recorder.finish(symbols),
            Inner::Plain(recorder) => recorder.finish(symbols),
        }
    }
}

/// Running the child with no profiler at all.
mod plain {
    use std::process::{Child, Command};

    use crate::model::{Profile, View};
    use crate::symbols::KiraSymbols;

    use super::{CollectError, Launch};

    /// A child running with nothing watching it.
    #[derive(Debug)]
    pub(super) struct Recorder {
        child: Child,
    }

    impl Recorder {
        /// The name this collector reports for itself.
        pub(super) const TOOL: &'static str = "none";

        pub(super) fn start(launch: &Launch) -> Result<Self, CollectError> {
            let mut command = Command::new(&launch.program);
            command.args(&launch.arguments);
            for (key, value) in &launch.environment {
                command.env(key, value);
            }
            let child = command.spawn().map_err(|source| CollectError::Spawn {
                program: launch.program.clone(),
                source,
            })?;
            Ok(Self { child })
        }

        pub(super) fn wait(&mut self) -> Result<i32, CollectError> {
            let status = self.child.wait().map_err(|source| CollectError::Io {
                action: "waiting for the program".to_owned(),
                source,
            })?;
            Ok(status.code().unwrap_or(1))
        }

        pub(super) fn finish(self, _symbols: &KiraSymbols) -> Result<Profile, CollectError> {
            Ok(Profile::new(View::Machine, "none", Self::TOOL))
        }
    }
}

/// A platform with no profiler this build knows how to drive.
#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    all(windows, any(target_arch = "x86_64", target_arch = "aarch64"))
)))]
mod unsupported {
    use crate::model::Profile;
    use crate::symbols::KiraSymbols;

    use super::{CollectError, CollectOptions, Launch};

    /// Refuses at the start, so no caller ever holds one.
    #[derive(Debug)]
    pub(super) enum Recorder {}

    impl Recorder {
        pub(super) const TOOL: &'static str = "none";

        pub(super) fn start(
            _launch: &Launch,
            _options: &CollectOptions,
        ) -> Result<Self, CollectError> {
            Err(CollectError::Unavailable {
                tool: Self::TOOL,
                reason: "this platform has no profiler Kira knows how to drive".to_owned(),
            })
        }

        pub(super) fn wait(&mut self) -> Result<i32, CollectError> {
            match *self {}
        }

        pub(super) fn finish(self, _symbols: &KiraSymbols) -> Result<Profile, CollectError> {
            match self {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_column_is_the_programs_own_name() {
        let launch = Launch::new(PathBuf::from("/tmp/build/hello.exe"));
        assert_eq!(launch.command(), "hello");
    }

    #[test]
    fn a_run_hosted_by_kira_is_named_after_the_program_it_hosts() {
        let launch = Launch {
            label: Some("grid".to_owned()),
            ..Launch::new(PathBuf::from("/usr/bin/kira"))
        };
        assert_eq!(launch.command(), "grid");
    }

    #[test]
    fn every_platform_names_the_profiler_it_would_drive() {
        assert!(!MachineRecorder::tool().is_empty());
    }
}
