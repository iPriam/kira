//! Instrumentation and profiling model for run sessions.
//!
//! Layer 8 of the Kira package graph.
//! Ported from kira-zig `packages/kira_instruments` (single-module package).
//! The report model types are ported; the human/JSON writers and the
//! sampling loop land with the port.

// #![warn(missing_docs)] // enable once the port lands real code

/// What an instruments session tracks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstrumentKind {
    Memory,
    Cpu,
}

impl InstrumentKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::Cpu => "cpu",
        }
    }
}

/// The execution backend the instrumented process ran on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstrumentBackend {
    Runtime,
    Llvm,
    Hybrid,
}

impl InstrumentBackend {
    pub fn label(self) -> &'static str {
        match self {
            Self::Runtime => "runtime",
            Self::Llvm => "llvm",
            Self::Hybrid => "hybrid",
        }
    }
}

/// Pass/fail outcome of an instruments run or a single track.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InstrumentStatus {
    #[default]
    Pass,
    Fail,
}

impl InstrumentStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
        }
    }
}

/// Why an instruments run failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstrumentFailureKind {
    MemoryGrowthExceeded,
    ProcessExitFailed,
    Timeout,
    InvalidConfiguration,
}

impl InstrumentFailureKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::MemoryGrowthExceeded => "memory_growth_exceeded",
            Self::ProcessExitFailed => "process_exit_failed",
            Self::Timeout => "timeout",
            Self::InvalidConfiguration => "invalid_configuration",
        }
    }
}

/// How the instrumented process ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessEndReason {
    Exited,
    DurationCompleted,
}

impl ProcessEndReason {
    pub fn label(self) -> &'static str {
        match self {
            Self::Exited => "exited",
            Self::DurationCompleted => "duration_completed",
        }
    }
}

/// A single failure reason attached to a report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailureReason {
    pub kind: InstrumentFailureKind,
    pub message: String,
}

/// Memory track results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryReport {
    pub metric: String,
    pub rss_start_bytes: u64,
    pub rss_end_bytes: u64,
    pub rss_peak_bytes: u64,
    pub rss_growth_bytes: i64,
    pub fail_on_growth_bytes: Option<u64>,
    pub sample_count: usize,
    pub status: InstrumentStatus,
}

impl Default for MemoryReport {
    fn default() -> Self {
        Self {
            metric: "private_working_set".to_string(),
            rss_start_bytes: 0,
            rss_end_bytes: 0,
            rss_peak_bytes: 0,
            rss_growth_bytes: 0,
            fail_on_growth_bytes: None,
            sample_count: 0,
            status: InstrumentStatus::Pass,
        }
    }
}

/// CPU track results.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CpuReport {
    pub available: bool,
    pub average_percent: Option<f64>,
    pub peak_percent: Option<f64>,
    pub sample_count: usize,
}

/// Instrumented process lifecycle results.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessReport {
    pub pid: Option<u32>,
    pub end_reason: ProcessEndReason,
    pub exit_code: Option<u8>,
}

/// The full report of one `kira instruments run` session.
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    pub command: String,
    pub target: String,
    pub backend: InstrumentBackend,
    pub tracks: Vec<InstrumentKind>,
    pub duration_seconds: f64,
    pub sample_rate_hz: f64,
    pub samples: usize,
    pub process: ProcessReport,
    pub memory: Option<MemoryReport>,
    pub cpu: Option<CpuReport>,
    pub status: InstrumentStatus,
    pub failure_reasons: Vec<FailureReason>,
}
