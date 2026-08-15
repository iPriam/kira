//! Running a program under the profiler, from both ends.
//!
//! A recording has two sides. The **parent** starts the program under the
//! platform's profiler and collects the machine view; the **child** — which for
//! a VM or hybrid run is `kira` itself — samples the Kira call stack from
//! inside and writes it out. This module owns both, and the agreement between
//! them: two environment variables and a file.
//!
//! The child writes its half as an ordinary trace holding one profile, so there
//! is exactly one serialized format in the profiler and the parent reads its
//! child's samples with the same code that reads a finished recording.

use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::collect::{CollectError, CollectOptions, Launch, MachineRecorder};
use crate::model::{Nanos, Profile, View};
use crate::runtime::RuntimeSampler;
use crate::symbols::KiraSymbols;
use crate::trace::{Trace, TraceError, TraceMeta};

/// Names the file a profiled child writes its Kira view to.
pub const SAMPLES_VARIABLE: &str = "KIRA_PROFILE_SAMPLES";

/// Names the frequency, in hertz, a profiled child samples itself at.
pub const FREQUENCY_VARIABLE: &str = "KIRA_PROFILE_HZ";

/// Why a recording failed.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// The machine samples could not be collected.
    #[error(transparent)]
    Collect(#[from] CollectError),
    /// A trace could not be read or written.
    #[error(transparent)]
    Trace(#[from] TraceError),
}

/// What a recording should collect.
#[derive(Debug, Clone)]
pub struct RecordOptions {
    /// What to ask the platform profiler for.
    pub collect: CollectOptions,
    /// Whether the child should sample its own Kira call stack.
    ///
    /// False for a native run, whose machine frames already are Kira frames.
    pub kira_view: bool,
    /// Where the child writes its Kira view, when it collects one.
    pub kira_samples: PathBuf,
    /// The arguments the *program* was given.
    ///
    /// Not the child's: a VM run's child is `kira run --backend vm app.kira`,
    /// and a reader of that recording wants to know what the program was asked
    /// to do, not how the profiler started it.
    pub arguments: Vec<String>,
}

/// A finished recording.
#[derive(Debug)]
pub struct RecordOutcome {
    /// Everything that was recorded.
    pub trace: Trace,
    /// The exit code the program reported.
    pub exit_code: i32,
    /// What a reader should know about how the recording went.
    pub notes: Vec<String>,
}

/// Runs `launch` under the profiler and returns everything it recorded.
///
/// A platform with no profiler this build can drive is not a failed recording:
/// the program still runs, the Kira view is still collected, and the reason the
/// machine view is missing comes back as a note. A native run has no Kira view
/// to fall back on, so there the same condition is a failure — which the caller
/// decides by whether it asked for one.
pub fn record(
    launch: &Launch,
    options: &RecordOptions,
    symbols: &KiraSymbols,
) -> Result<RecordOutcome, SessionError> {
    let mut notes = Vec::new();
    let mut launch = launch.clone();
    if options.kira_view {
        launch.environment.push((
            SAMPLES_VARIABLE.to_owned(),
            options.kira_samples.to_string_lossy().into_owned(),
        ));
        launch.environment.push((
            FREQUENCY_VARIABLE.to_owned(),
            options.collect.frequency.to_string(),
        ));
        let _ = std::fs::remove_file(&options.kira_samples);
    }

    let started = unix_millis();
    let clock = Instant::now();
    let mut recorder = match MachineRecorder::start(&launch, &options.collect) {
        Ok(recorder) => recorder,
        Err(error) if options.kira_view => {
            notes.push(format!(
                "no machine view: {error}\nnote: the Kira view is unaffected — it is sampled \
                 inside the program rather than by {}",
                MachineRecorder::tool()
            ));
            MachineRecorder::start_unprofiled(&launch)?
        }
        Err(error) => return Err(error.into()),
    };
    let exit_code = recorder.wait()?;
    let duration = Nanos::new(clock.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64);

    let mut machine = recorder.finish(symbols)?;

    let mut profiles = Vec::new();
    if options.kira_view {
        match read_child_view(&options.kira_samples) {
            Ok(Some((kira, kira_started))) => {
                let dropped = trim_to_run(&mut machine, &kira, started, kira_started);
                if dropped > 0 {
                    notes.push(format!(
                        "machine view: {dropped} samples taken before or after the program ran \
                         were dropped, which is the compile that preceded it"
                    ));
                }
                profiles.push(kira);
            }
            Ok(None) => notes.push(
                "no Kira view: the program wrote no samples, which means it finished before the \
                 first one was due"
                    .to_owned(),
            ),
            Err(error) => notes.push(format!("no Kira view: {error}")),
        }
    }
    profiles.push(machine);

    Ok(RecordOutcome {
        trace: Trace {
            meta: TraceMeta {
                command: launch.command(),
                arguments: options.arguments.clone(),
                backend: symbols.backend(),
                source: symbols.source().map(Path::to_path_buf),
                started_unix_ms: started,
                duration,
                exit_code,
            },
            profiles,
        },
        exit_code,
        notes,
    })
}

/// Now, in milliseconds since the Unix epoch.
fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

/// Reads the Kira view a child wrote, and when it started writing it.
fn read_child_view(path: &Path) -> Result<Option<(Profile, u64)>, TraceError> {
    if !path.is_file() {
        return Ok(None);
    }
    let mut trace = Trace::load(path)?;
    let index = trace
        .profiles
        .iter()
        .position(|profile| profile.view == View::Kira);
    let started = trace.meta.started_unix_ms;
    Ok(index.map(|index| (trace.profiles.remove(index), started)))
}

/// How far either side of the run a machine sample is still kept.
const RUN_MARGIN: Nanos = Nanos::new(100_000_000);

/// Drops machine samples taken outside the window the program was running in,
/// returning how many were dropped.
///
/// The child is `kira`, so it compiles before it runs, and every sample of that
/// compile is time the report would otherwise attribute to the program. The two
/// samplers measure from different origins — the parent from the moment it
/// started the child, the child from the moment it began sampling itself — so
/// the window is put on one clock through the wall-clock time each of them
/// recorded when it started.
fn trim_to_run(machine: &mut Profile, kira: &Profile, parent_ms: u64, child_ms: u64) -> usize {
    let (Some(first), Some(last)) = (kira.samples.first(), kira.samples.last()) else {
        return 0;
    };
    let offset = child_ms.saturating_sub(parent_ms).saturating_mul(1_000_000);
    let start = Nanos::new(
        first
            .time
            .get()
            .saturating_add(offset)
            .saturating_sub(RUN_MARGIN.get()),
    );
    let end = Nanos::new(
        last.time
            .get()
            .saturating_add(offset)
            .saturating_add(RUN_MARGIN.get()),
    );
    let before = machine.samples.len();
    machine
        .samples
        .retain(|sample| sample.time >= start && sample.time <= end);
    before - machine.samples.len()
}

/// The Kira-level sampler a profiled child runs on itself.
#[derive(Debug)]
pub struct ChildSampler {
    sampler: RuntimeSampler,
    path: PathBuf,
    clock: Instant,
    started_unix_ms: u64,
}

impl ChildSampler {
    /// Starts sampling when this process was started by a recording.
    ///
    /// Returns `None` in every ordinary run, which is what keeps the whole
    /// mechanism invisible to a program nobody is profiling.
    #[must_use]
    pub fn start() -> Option<Self> {
        let path = std::env::var_os(SAMPLES_VARIABLE)?;
        if path.is_empty() {
            return None;
        }
        let frequency = std::env::var(FREQUENCY_VARIABLE)
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(crate::collect::DEFAULT_FREQUENCY);
        let clock = Instant::now();
        Some(Self {
            sampler: RuntimeSampler::start(frequency, clock),
            path: PathBuf::from(path),
            clock,
            started_unix_ms: unix_millis(),
        })
    }

    /// Stops sampling and writes the Kira view for the parent to collect.
    ///
    /// The wall-clock moment sampling began is written with it: it is the only
    /// thing that puts the child's sample times and the parent's on one clock.
    pub fn finish(self, symbols: &KiraSymbols) -> Result<(), SessionError> {
        let duration = Nanos::new(self.clock.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64);
        let profile = self.sampler.finish().into_profile(symbols);
        let trace = Trace {
            meta: TraceMeta {
                command: String::new(),
                arguments: Vec::new(),
                backend: symbols.backend(),
                source: symbols.source().map(Path::to_path_buf),
                started_unix_ms: self.started_unix_ms,
                duration,
                exit_code: 0,
            },
            profiles: vec![profile],
        };
        trace.save(&self.path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_process_nobody_is_profiling_starts_no_sampler() {
        // The variable is absent in an ordinary test process, which is exactly
        // the condition every ordinary run is in.
        assert!(std::env::var_os(SAMPLES_VARIABLE).is_none());
        assert!(ChildSampler::start().is_none());
    }

    #[test]
    fn a_missing_child_view_is_absent_rather_than_an_error() {
        let path = std::env::temp_dir().join("kira-profile-missing-child-view.kira-profile");
        let _ = std::fs::remove_file(&path);
        assert!(matches!(read_child_view(&path), Ok(None)));
    }
}
