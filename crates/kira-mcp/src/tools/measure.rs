//! `kira_dev_benchmark` and `kira_dev_fuzz`: performance, and stress.
//!
//! Both refuse to invent a result. A benchmark of something that was never run
//! and a fuzz campaign with no targets are the two easiest places in this
//! server to report a comfortable number, so both check what exists first and
//! report a missing capability when the answer is that nothing does.

use serde_json::{Value, json};

use super::program::{self, Configuration};
use super::{
    BACKENDS, DEVICES, enum_field, environment, string_field, string_list, timeout, uint_field,
};
use crate::exec::{self, Run};
use crate::schema::{Diagnostic, Failure, FailureKind, capability_missing};

/// The most iterations one benchmark call will run.
const MAX_ITERATIONS: u64 = 200;

pub fn benchmark_descriptor() -> Value {
    json!({
        "name": "kira_dev_benchmark",
        "description": "Measure how long a Kira program takes to run under each engine, and report the measured times.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "source": { "type": "string" },
                "file": { "type": "string" },
                "arguments": { "type": "array", "items": { "type": "string" } },
                "backend": { "type": "string", "enum": BACKENDS },
                "against": { "type": "string", "enum": BACKENDS },
                "device": { "type": "string", "enum": DEVICES },
                "iterations": { "type": "integer", "minimum": 1, "maximum": MAX_ITERATIONS },
                "warmup": { "type": "integer", "minimum": 0 },
                "baseline_seconds": { "type": "number", "exclusiveMinimum": 0 },
                "tolerance": { "type": "number", "minimum": 0 },
                "environment": { "type": "object", "additionalProperties": { "type": "string" } },
                "timeout": { "type": "integer", "minimum": 1 }
            },
            "required": ["source"]
        }
    })
}

pub fn fuzz_descriptor() -> Value {
    json!({
        "name": "kira_dev_fuzz",
        "description": "Run a fuzzing campaign against a compiler or runtime target and report the inputs that crashed it.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "target": { "type": "string" },
                "corpus": { "type": "string" },
                "runs": { "type": "integer", "minimum": 1 },
                "timeout": { "type": "integer", "minimum": 1 }
            }
        }
    })
}

pub fn benchmark(arguments: &Value) -> (Value, bool) {
    let source = match (
        string_field(arguments, "source"),
        string_field(arguments, "file"),
    ) {
        (Err(rejection), _) | (_, Err(rejection)) => return rejection,
        (Ok(source), Ok(file)) => match source.or(file) {
            Some(source) => source.to_owned(),
            None => return super::invalid("source", "a Kira source file is required"),
        },
    };
    let device = match enum_field(arguments, "device", &DEVICES, Some("host")) {
        Ok(device) => device.unwrap_or("host"),
        Err(rejection) => return rejection,
    };
    let primary = match enum_field(arguments, "backend", &BACKENDS, Some("vm")) {
        Ok(backend) => backend.unwrap_or("vm"),
        Err(rejection) => return rejection,
    };
    let against = match enum_field(arguments, "against", &BACKENDS, None) {
        Ok(against) => against,
        Err(rejection) => return rejection,
    };
    let iterations = match uint_field(arguments, "iterations", 5) {
        Ok(0) => return super::invalid("iterations", "must be at least one"),
        Ok(iterations) if iterations > MAX_ITERATIONS => {
            return super::invalid("iterations", &format!("must be at most {MAX_ITERATIONS}"));
        }
        Ok(iterations) => iterations,
        Err(rejection) => return rejection,
    };
    let warmup = match uint_field(arguments, "warmup", 1) {
        Ok(warmup) => warmup,
        Err(rejection) => return rejection,
    };
    let program_args = match string_list(arguments, "arguments") {
        Ok(args) => args,
        Err(rejection) => return rejection,
    };
    let env = match environment(arguments) {
        Ok(env) => env,
        Err(rejection) => return rejection,
    };
    let bound = match timeout(arguments) {
        Ok(bound) => bound,
        Err(rejection) => return rejection,
    };
    let baseline = match float_field(arguments, "baseline_seconds") {
        Ok(baseline) => baseline,
        Err(rejection) => return rejection,
    };
    let tolerance = match float_field(arguments, "tolerance") {
        Ok(tolerance) => tolerance.unwrap_or(0.10),
        Err(rejection) => return rejection,
    };

    let mut configurations = vec![Configuration::new(primary, device)];
    if let Some(against) = against.filter(|against| *against != primary) {
        configurations.push(Configuration::new(against, device));
    }

    let mut measurements = Vec::new();
    let mut failures: Vec<Failure> = Vec::new();
    let mut runs: Vec<Run> = Vec::new();

    for configuration in &configurations {
        let mut samples: Vec<f64> = Vec::new();
        let mut failed = false;
        // Warm-up runs are discarded rather than averaged in: the first run of a
        // native build pays for the build, which is not what was being measured.
        for index in 0..(warmup + iterations) {
            let run = match program::run(configuration, &source, &program_args, &env, bound) {
                Ok(run) => run,
                Err(error) => {
                    failures.push(Failure::new(FailureKind::Crash, error.to_string()));
                    failed = true;
                    break;
                }
            };
            if !run.success() {
                // A timing taken from a run that failed measures the failure.
                let mut failure = Failure::new(
                    match run.timed_out {
                        true => FailureKind::Timeout,
                        false => FailureKind::Crash,
                    },
                    format!(
                        "the program did not run successfully under `{}`, so it was not timed",
                        configuration.label()
                    ),
                )
                .with_run(&run);
                failure.backend = Some(configuration.backend.clone());
                failures.push(failure);
                runs.push(run);
                failed = true;
                break;
            }
            if index >= warmup {
                samples.push(run.duration_seconds);
            }
            runs.push(run);
        }
        if failed {
            continue;
        }
        let summary = statistics(configuration, &samples);
        // A regression is only claimed against a baseline the caller supplied.
        // Without one there is nothing to be slower than, and a tool that
        // invented a budget would report regressions nobody set.
        if let Some(baseline) = baseline {
            let median = summary["median_seconds"].as_f64().unwrap_or_default();
            let allowed = baseline * (1.0 + tolerance);
            if median > allowed {
                let mut failure = Failure::new(
                    FailureKind::PerformanceRegression,
                    format!(
                        "`{}` took a median of {median:.3}s against a baseline of {baseline:.3}s \
                         with a {:.0}% tolerance",
                        configuration.label(),
                        tolerance * 100.0
                    ),
                );
                failure.backend = Some(configuration.backend.clone());
                failure.target = Some(configuration.device.clone());
                failures.push(failure);
            }
        }
        measurements.push(summary);
    }

    let success = failures.is_empty();
    (
        json!({
            "success": success,
            "iterations": iterations,
            "warmup": warmup,
            "source": source,
            "measurements": measurements,
            "failures": failures,
            "diagnostics": [Diagnostic::message(
                "note",
                "times are whole-process wall clock, including toolchain startup; \
                 they compare engines against each other, not against an absolute budget",
            )],
            "commands": runs.iter().map(exec::run_json).collect::<Vec<_>>(),
            "stdout": runs.last().map(|run| run.stdout.clone()).unwrap_or_default(),
            "stderr": runs.last().map(|run| run.stderr.clone()).unwrap_or_default(),
        }),
        !success,
    )
}

/// Reads an optional number field.
fn float_field(arguments: &Value, field: &str) -> Result<Option<f64>, (Value, bool)> {
    match arguments.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(number)) => match number.as_f64() {
            Some(value) if value >= 0.0 => Ok(Some(value)),
            _ => Err(super::invalid(field, "expected a non-negative number")),
        },
        Some(_) => Err(super::invalid(field, "expected a number")),
    }
}

/// The summary of one configuration's samples.
///
/// Minimum and median are reported beside the mean because a single scheduling
/// stall moves a mean of five samples further than it moves the truth.
fn statistics(configuration: &Configuration, samples: &[f64]) -> Value {
    let mut sorted = samples.to_vec();
    sorted.sort_by(|left, right| left.total_cmp(right));
    let count = sorted.len();
    let median = match count {
        0 => 0.0,
        count if count % 2 == 1 => sorted[count / 2],
        count => (sorted[count / 2 - 1] + sorted[count / 2]) / 2.0,
    };
    json!({
        "configuration": configuration.json(),
        "samples": samples,
        "minimum_seconds": sorted.first().copied().unwrap_or_default(),
        "median_seconds": median,
        "maximum_seconds": sorted.last().copied().unwrap_or_default(),
        "mean_seconds": match count {
            0 => 0.0,
            count => sorted.iter().sum::<f64>() / count as f64,
        },
    })
}

pub fn fuzz(arguments: &Value) -> (Value, bool) {
    let target = match string_field(arguments, "target") {
        Ok(target) => target,
        Err(rejection) => return rejection,
    };
    let available = fuzz_targets();
    if available.is_empty() {
        // No campaign was run, so there are no crashes to report. Saying "zero
        // crashes found" here would be read as the fuzzer having cleared the
        // target, which it did not, because it does not exist.
        return capability_missing(
            "fuzz",
            "this repository defines no fuzz targets: there is no `fuzz/` directory \
             and no `cargo-fuzz` setup, so no campaign can be run",
        );
    }
    match target {
        Some(target) if !available.iter().any(|known| known == target) => {
            super::invalid("target", &format!("expected one of {available:?}"))
        }
        _ => capability_missing(
            "fuzz",
            "fuzz targets exist but this server has no runner wired to them yet",
        ),
    }
}

/// The fuzz targets this repository defines.
///
/// Discovered rather than assumed, so the day a `fuzz/` directory lands the
/// answer changes with it instead of staying a hardcoded "none".
fn fuzz_targets() -> Vec<String> {
    let directory = exec::repository_root().join("fuzz/fuzz_targets");
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            match path.extension().and_then(|extension| extension.to_str()) {
                Some("rs") => path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .map(str::to_owned),
                _ => None,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The honest answer when there is nothing to fuzz.
    #[test]
    fn fuzzing_a_repository_with_no_targets_reports_a_missing_capability() {
        let (value, is_error) = fuzz(&json!({}));
        match fuzz_targets().is_empty() {
            true => {
                assert!(is_error, "an unrun campaign is not a success");
                assert_eq!(value["failures"][0]["kind"], json!("capability_missing"));
            }
            false => assert!(value["success"].is_boolean()),
        }
    }

    #[test]
    fn a_benchmark_without_a_source_is_refused() {
        assert!(benchmark(&json!({ "iterations": 3 })).1);
    }

    #[test]
    fn an_iteration_count_beyond_the_bound_is_refused() {
        let (_, is_error) = benchmark(&json!({
            "source": "m.kira", "iterations": MAX_ITERATIONS + 1
        }));
        assert!(is_error);
    }

    /// The median of an even sample count averages the middle pair.
    #[test]
    fn statistics_report_the_spread_and_not_only_a_mean() {
        let summary = statistics(&Configuration::new("vm", "host"), &[4.0, 1.0, 3.0, 2.0]);
        assert_eq!(summary["minimum_seconds"], json!(1.0));
        assert_eq!(summary["median_seconds"], json!(2.5));
        assert_eq!(summary["maximum_seconds"], json!(4.0));
        assert_eq!(summary["mean_seconds"], json!(2.5));
    }

    /// An empty sample set reports zeroes rather than dividing by nothing.
    #[test]
    fn no_samples_produce_no_timings() {
        let summary = statistics(&Configuration::new("vm", "host"), &[]);
        assert_eq!(summary["mean_seconds"], json!(0.0));
    }
}
