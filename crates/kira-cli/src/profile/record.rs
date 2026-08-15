//! `kira profile record`: run the program and write a recording.
//!
//! A sampled recording runs the program as a **child process** and profiles
//! that child, on every platform and every backend. For a native build the
//! child is the built executable; for a VM or hybrid build it is `kira` itself,
//! told through the environment to sample the Kira call stack from inside.
//!
//! One child, always, is what makes the three platform collectors the same
//! program: none of them needs to attach to a running process, none needs an
//! elevated session, and the compile that precedes the run is somebody else's
//! process rather than noise in the middle of the profile.
//!
//! The `instructions` event is the exception, and it is one on purpose: exact
//! counting is the interpreter observing itself, so it runs here rather than in
//! a child.

use std::path::PathBuf;

use kira_backend_api::BackendMode;
use kira_debug::DebugInfo;
use kira_profile::collect::{CollectOptions, Launch};
use kira_profile::session::{RecordOptions, record};
use kira_profile::symbols::KiraSymbols;
use kira_profile::trace::{Trace, TraceMeta};
use kira_profile::{MachineRecorder, Nanos};

use crate::options::{CompileOptions, Device};
use crate::pipeline::{EXIT_FAILURE, EXIT_OK, EXIT_USAGE};
use crate::progress::{err, out};

/// What `record` was asked to do.
#[derive(Debug, Clone)]
struct RecordArgs {
    compile: CompileOptions,
    collect: CollectOptions,
    event: Event,
    output: PathBuf,
}

/// Which event a recording counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Event {
    /// Sampled by the platform's profiler, and by the runtime for Kira frames.
    Sampled,
    /// Every interpreted instruction, counted exactly.
    Instructions,
}

/// Runs `kira profile record`.
pub(super) fn run(args: &[String]) -> i32 {
    let parsed = match parse(args) {
        Ok(parsed) => parsed,
        Err(code) => return code,
    };
    let surface = crate::progress::Surface::install("Recording");
    let _guard = crate::progress::Finish(surface);

    let compile_args = args_for_compile(args);
    let (mut options, compiled) =
        match crate::pipeline::command_inputs("profile record", &compile_args) {
            Ok(inputs) => inputs,
            Err(code) => return code,
        };
    options.program_arguments = parsed.compile.program_arguments.clone();
    if !matches!(options.device, Device::Host) {
        err!(
            "kira profile record: profiling runs on this machine; a Web build has no process \
             here to sample"
        );
        return EXIT_USAGE;
    }
    let ir = match crate::pipeline::entrypoint_ir("profile record", compiled) {
        Ok(ir) => ir,
        Err(code) => return code,
    };
    let source = PathBuf::from(&options.path);
    let stem = source
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "program".to_owned());
    let info = DebugInfo::from_ir(
        &ir,
        stem,
        crate::debugger::backend(options.backend),
        Some(&source),
    )
    .optimized(options.release);
    let symbols = KiraSymbols::from_debug(&info);

    let foreign = match crate::pipeline::foreign_inputs(&options.path, &ir, options.device) {
        Ok(foreign) => foreign,
        Err(code) => return code,
    };
    let link = crate::pipeline::foreign_link_of(&foreign);

    if parsed.event == Event::Instructions {
        return record_instructions(&parsed, &options, &ir, link, &symbols);
    }

    let launch = match launch_for(&options, &ir, link, &info) {
        Ok(launch) => launch,
        Err(code) => return code,
    };
    let kira_view = !matches!(options.backend, BackendMode::LlvmNative);
    let record_options = RecordOptions {
        collect: parsed.collect,
        kira_view,
        kira_samples: samples_path(&source),
        arguments: options.program_arguments.clone(),
    };
    out!(
        "recording {} on {} with {}",
        launch.command(),
        options.backend.label(),
        MachineRecorder::tool()
    );
    let outcome = match record(&launch, &record_options, &symbols) {
        Ok(outcome) => outcome,
        Err(error) => {
            err!("kira profile record: {error}");
            return EXIT_FAILURE;
        }
    };
    for note in &outcome.notes {
        err!("kira profile record: {note}");
    }
    if let Err(error) = outcome.trace.save(&parsed.output) {
        err!("kira profile record: {error}");
        return EXIT_FAILURE;
    }
    report_written(&outcome.trace, &parsed.output);
    outcome.exit_code
}

/// Records the exact instruction count instead of sampling.
fn record_instructions(
    parsed: &RecordArgs,
    options: &CompileOptions,
    ir: &kira_ir::IrProgram,
    link: &kira_llvm_backend::NativeLinkInputs,
    symbols: &KiraSymbols,
) -> i32 {
    if matches!(options.backend, BackendMode::LlvmNative) {
        err!(
            "kira profile record: `-e instructions` counts interpreted instructions, and a \
             native build has none\n\
             note: record the default `cpu-clock` event on this backend, or count instructions \
             with `--backend vm`"
        );
        return EXIT_USAGE;
    }
    let source = PathBuf::from(&options.path);
    let started = std::time::Instant::now();
    let counted = match super::instructions::count(
        ir,
        &source,
        options.backend,
        link,
        &options.program_arguments,
        options.emit_llvm_ir,
    ) {
        Ok(counted) => counted,
        Err(code) => return code,
    };
    let duration = Nanos::new(started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64);
    let trace = Trace {
        meta: TraceMeta {
            command: source
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_else(|| "program".to_owned()),
            arguments: options.program_arguments.clone(),
            backend: crate::debugger::backend(options.backend),
            source: Some(source),
            started_unix_ms: 0,
            duration,
            exit_code: 0,
        },
        profiles: vec![counted.into_profile(symbols)],
    };
    if let Err(error) = trace.save(&parsed.output) {
        err!("kira profile record: {error}");
        return EXIT_FAILURE;
    }
    report_written(&trace, &parsed.output);
    EXIT_OK
}

/// The child a sampled recording profiles.
fn launch_for(
    options: &CompileOptions,
    ir: &kira_ir::IrProgram,
    link: &kira_llvm_backend::NativeLinkInputs,
    info: &DebugInfo,
) -> Result<Launch, i32> {
    if matches!(options.backend, BackendMode::LlvmNative) {
        // Built with debug records, because the machine view is the whole
        // recording for a native run and an unsymbolized address names nothing.
        let artifacts = crate::native::build_debug(
            ir,
            std::path::Path::new(&options.path),
            options.emit_llvm_ir,
            options.release,
            link,
            info,
        )
        .map_err(|error| {
            err!("kira profile record: {error}");
            EXIT_FAILURE
        })?;
        let executable = artifacts.executable.ok_or_else(|| {
            err!("kira profile record: the native build produced no executable");
            EXIT_FAILURE
        })?;
        return Ok(Launch {
            program: executable,
            arguments: options.program_arguments.clone(),
            environment: Vec::new(),
            label: Some(info.module_name.clone()),
        });
    }

    let kira = std::env::current_exe().map_err(|error| {
        err!("kira profile record: cannot locate this executable to run the program in: {error}");
        EXIT_FAILURE
    })?;
    let mut arguments = vec![
        "run".to_owned(),
        "--backend".to_owned(),
        options.backend.label().to_owned(),
        options.path.clone(),
    ];
    if options.release {
        arguments.push("--release".to_owned());
    }
    if options.emit_llvm_ir {
        arguments.push("--emit-llvm-ir".to_owned());
    }
    if !options.program_arguments.is_empty() {
        arguments.push("--".to_owned());
        arguments.extend(options.program_arguments.iter().cloned());
    }
    Ok(Launch {
        program: kira,
        arguments,
        environment: Vec::new(),
        label: Some(info.module_name.clone()),
    })
}

/// Where the child writes its Kira view: beside the program's other artifacts.
fn samples_path(source: &std::path::Path) -> PathBuf {
    kira_project::build_directory(source).join("profile-samples.kira-profile")
}

fn report_written(trace: &Trace, output: &std::path::Path) {
    let samples = trace
        .profiles
        .iter()
        .map(|profile| profile.samples.len())
        .sum::<usize>();
    out!(
        "wrote {} ({samples} samples across {} view(s))\nread it with `kira profile report`",
        output.display(),
        trace.profiles.len(),
    );
}

/// The arguments `CompileOptions` should see: everything `record` did not take.
fn args_for_compile(args: &[String]) -> Vec<String> {
    let mut kept = Vec::with_capacity(args.len());
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-F" | "--freq" | "-e" | "--event" | "-o" | "--output" | "--max-depth" => index += 1,
            "-g" | "--call-graph" | "--no-call-graph" => {}
            "--" => {
                kept.extend(args[index..].iter().cloned());
                break;
            }
            other => kept.push(other.to_owned()),
        }
        index += 1;
    }
    kept
}

/// Parses the flags `record` owns.
fn parse(args: &[String]) -> Result<RecordArgs, i32> {
    let mut collect = CollectOptions::default();
    let mut event = Event::Sampled;
    let mut output = PathBuf::from(super::DEFAULT_TRACE);
    let mut index = 0;
    while index < args.len() {
        let argument = args[index].as_str();
        if argument == "--" {
            break;
        }
        let mut value = |name: &str| -> Result<String, i32> {
            index += 1;
            args.get(index).cloned().ok_or_else(|| {
                err!("kira profile record: `{name}` expects a value");
                EXIT_USAGE
            })
        };
        match argument {
            "-F" | "--freq" => {
                let word = value("-F")?;
                let hertz = super::number("record", "-F", &word)?;
                if hertz == 0 || hertz > 100_000 {
                    err!("kira profile record: `-F` expects a frequency between 1 and 100000 Hz");
                    return Err(EXIT_USAGE);
                }
                collect.frequency = hertz as u32;
            }
            "--max-depth" => {
                let word = value("--max-depth")?;
                collect.max_depth = super::number("record", "--max-depth", &word)?.max(1) as u32;
            }
            "-g" | "--call-graph" => collect.call_graph = true,
            "--no-call-graph" => collect.call_graph = false,
            "-o" | "--output" => output = PathBuf::from(value("-o")?),
            "-e" | "--event" => {
                let word = value("-e")?;
                event = match word.as_str() {
                    "cpu-clock" | "wall-clock" | "sampled" => Event::Sampled,
                    "instructions" => Event::Instructions,
                    other => {
                        err!(
                            "kira profile record: unknown event `{other}`; \
                             expected cpu-clock or instructions"
                        );
                        return Err(EXIT_USAGE);
                    }
                };
            }
            _ => {}
        }
        index += 1;
    }
    let compile = CompileOptions::parse(&args_for_compile(args)).map_err(|error| {
        err!("kira profile record: {error}");
        EXIT_USAGE
    })?;
    Ok(RecordArgs {
        compile,
        collect,
        event,
        output,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| (*word).to_owned()).collect()
    }

    #[test]
    fn recording_flags_are_taken_before_the_compiler_sees_the_rest() {
        let raw = args(&[
            "app",
            "-F",
            "500",
            "-e",
            "instructions",
            "-o",
            "run.profile",
            "--backend",
            "vm",
            "--",
            "--rows",
            "3",
        ]);
        let parsed = parse(&raw).expect("the flags parse");
        assert_eq!(parsed.collect.frequency, 500);
        assert_eq!(parsed.event, Event::Instructions);
        assert_eq!(parsed.output, PathBuf::from("run.profile"));
        assert_eq!(parsed.compile.path, "app");
        assert_eq!(parsed.compile.program_arguments, vec!["--rows", "3"]);
    }

    #[test]
    fn the_default_frequency_is_the_one_a_report_names() {
        let parsed = parse(&args(&["app"])).expect("a bare path parses");
        assert_eq!(
            parsed.collect.frequency,
            kira_profile::collect::DEFAULT_FREQUENCY
        );
        assert!(parsed.collect.call_graph);
        assert_eq!(parsed.output, PathBuf::from(super::super::DEFAULT_TRACE));
    }

    #[test]
    fn an_unknown_event_is_refused_by_name() {
        assert_eq!(parse(&args(&["-e", "branches"])).err(), Some(EXIT_USAGE));
    }
}
