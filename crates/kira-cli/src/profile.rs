//! `kira profile`: record where a program spends its time, and read it back.
//!
//! The verbs, the flags, and the output are `perf`'s wherever `perf` has an
//! answer, because a profiler is worth as much as the number of people and
//! tools that already know how to drive it. `record` runs the program and
//! writes one recording; `report`, `annotate`, `script`, `stat`, and `diff`
//! read recordings and never run anything.

use std::path::PathBuf;

use kira_profile::model::View;
use kira_profile::render::{ReportOptions, Sort};

use crate::pipeline::{EXIT_OK, EXIT_USAGE};
use crate::progress::err;

mod instructions;
mod read;
mod record;

/// The recording `record` writes and every other verb reads when no `-o`/`-i`
/// names another.
///
/// `perf` writes `perf.data` into the working directory and reads it back from
/// there; this is the same bargain under Kira's own name.
pub const DEFAULT_TRACE: &str = "kira.profile";

/// Runs `kira profile <verb>`.
pub fn profile(args: &[String]) -> i32 {
    let Some((verb, rest)) = args.split_first() else {
        usage();
        return EXIT_USAGE;
    };
    match verb.as_str() {
        "record" => record::run(rest),
        "report" => read::report(rest),
        "annotate" => read::annotate(rest),
        "script" => read::script(rest),
        "stat" => read::stat(rest),
        "diff" => read::diff(rest),
        "help" | "--help" | "-h" => {
            usage();
            EXIT_OK
        }
        other => {
            err!("kira profile: unknown verb `{other}`");
            usage();
            EXIT_USAGE
        }
    }
}

/// Prints what `kira profile` accepts.
pub fn usage() {
    let paint = kira_toolchain::Paint::auto_stderr();
    eprintln!(
        "{} — sampled profiles of a running program",
        paint.bold("kira profile")
    );
    eprintln!();
    for (verb, arguments, note) in VERBS {
        eprintln!(
            "  {} {}{}",
            paint.cyan(&format!("kira profile {verb}")),
            arguments,
            paint.dim(&format!("\n      {note}")),
        );
    }
    eprintln!();
    eprintln!(
        "{}",
        paint.dim(&format!(
            "  Recordings default to `{DEFAULT_TRACE}` in the working directory.\n  \
             Views: `--kira` is the functions you wrote, `--machine` is what the machine ran."
        ))
    );
}

/// Every verb, its argument shape, and what it does.
const VERBS: [(&str, &str, &str); 6] = [
    (
        "record",
        "[file|dir] [--backend vm|llvm|hybrid] [-F <hz>] [-e cpu-clock|instructions] [-g] [-o <file>] [--release] [-- <args...>]",
        "run the program and write a recording",
    ),
    (
        "report",
        "[-i <file>] [--kira|--machine] [-g] [--no-children] [--sort self|children|symbol|dso] [--limit <n>] [--percent-limit <f>] [--thread <n>] [--symbol <text>] [--per-instruction] [--folded]",
        "where the time went, function by function",
    ),
    (
        "annotate",
        "<symbol> [-i <file>] [--kira|--machine] [--percent-limit <f>]",
        "where the time went inside one function",
    ),
    (
        "script",
        "[-i <file>] [--kira|--machine] [--thread <n>]",
        "every sample and its stack",
    ),
    ("stat", "[-i <file>]", "the one-screen summary of a run"),
    (
        "diff",
        "<baseline> <current> [--kira|--machine] [--limit <n>]",
        "what changed between two recordings",
    ),
];

/// What a reading verb was asked to show.
#[derive(Debug, Clone)]
pub(crate) struct ReadArgs {
    /// The recording to read.
    pub(crate) input: PathBuf,
    /// Which view to render.
    pub(crate) view: View,
    /// Whether the view was named explicitly.
    pub(crate) view_explicit: bool,
    /// How to render it.
    pub(crate) report: ReportOptions,
    /// Whatever was not a flag: a symbol, or the recordings to compare.
    pub(crate) positional: Vec<String>,
    /// Print collapsed stacks instead of a table.
    pub(crate) folded: bool,
}

impl ReadArgs {
    /// Parses the flags every reading verb shares.
    pub(crate) fn parse(verb: &str, args: &[String]) -> Result<Self, i32> {
        let mut parsed = ReadArgs {
            input: PathBuf::from(DEFAULT_TRACE),
            view: View::Kira,
            view_explicit: false,
            report: ReportOptions::default(),
            positional: Vec::new(),
            folded: false,
        };
        let mut index = 0;
        while index < args.len() {
            let argument = args[index].as_str();
            let mut value = |name: &str| -> Result<String, i32> {
                index += 1;
                args.get(index).cloned().ok_or_else(|| {
                    err!("kira profile {verb}: `{name}` expects a value");
                    EXIT_USAGE
                })
            };
            match argument {
                "-i" | "--input" => parsed.input = PathBuf::from(value("-i")?),
                "--kira" => {
                    parsed.view = View::Kira;
                    parsed.view_explicit = true;
                }
                "--machine" => {
                    parsed.view = View::Machine;
                    parsed.view_explicit = true;
                }
                "-g" | "--call-graph" => parsed.report.call_graph = true,
                "--no-call-graph" => parsed.report.call_graph = false,
                "--children" => parsed.report.children = true,
                "--no-children" => parsed.report.children = false,
                "--folded" => parsed.folded = true,
                "--per-instruction" => parsed.report.per_instruction = true,
                "--sort" => {
                    let word = value("--sort")?;
                    parsed.report.sort = Sort::parse(&word).ok_or_else(|| {
                        err!(
                            "kira profile {verb}: unknown sort `{word}`; \
                             expected self, children, symbol, or dso"
                        );
                        EXIT_USAGE
                    })?;
                }
                "--limit" => parsed.report.limit = number(verb, "--limit", &value("--limit")?)?,
                "--percent-limit" => {
                    let word = value("--percent-limit")?;
                    parsed.report.percent_limit = word.parse::<f64>().map_err(|_| {
                        err!("kira profile {verb}: `--percent-limit` expects a percentage");
                        EXIT_USAGE
                    })?;
                }
                "--thread" => {
                    let word = value("--thread")?;
                    let thread = number(verb, "--thread", &word)?;
                    parsed.report.thread = Some(kira_profile::model::ThreadId::new(thread as u32));
                }
                "--symbol" => parsed.report.symbol = Some(value("--symbol")?),
                other if other.starts_with('-') => {
                    err!("kira profile {verb}: unknown option `{other}`");
                    return Err(EXIT_USAGE);
                }
                other => parsed.positional.push(other.to_owned()),
            }
            index += 1;
        }
        Ok(parsed)
    }
}

/// Writes a rendered report to standard output.
///
/// A reader that stops reading is not an error. `kira profile script | head`
/// and `... | grep -m1` both close the pipe partway through, and a profiler
/// that answered a successful query with a panic would be unusable from exactly
/// the scripts it exists to serve.
pub(crate) fn emit(text: &str) -> i32 {
    use std::io::Write as _;

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    match out.write_all(text.as_bytes()).and_then(|()| out.flush()) {
        Ok(()) => EXIT_OK,
        Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => EXIT_OK,
        Err(error) => {
            err!("kira profile: cannot write the report: {error}");
            crate::pipeline::EXIT_FAILURE
        }
    }
}

/// Parses a non-negative count, reporting the flag it belonged to.
pub(crate) fn number(verb: &str, flag: &str, value: &str) -> Result<usize, i32> {
    value.parse::<usize>().map_err(|_| {
        err!("kira profile {verb}: `{flag}` expects a non-negative number");
        EXIT_USAGE
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| (*word).to_owned()).collect()
    }

    #[test]
    fn reading_flags_follow_the_spellings_perf_uses() {
        let parsed = ReadArgs::parse(
            "report",
            &args(&[
                "-i",
                "run.profile",
                "--machine",
                "-g",
                "--no-children",
                "--sort",
                "children",
                "--limit",
                "5",
                "--symbol",
                "Grid",
            ]),
        )
        .expect("the flags parse");
        assert_eq!(parsed.input, PathBuf::from("run.profile"));
        assert_eq!(parsed.view, View::Machine);
        assert!(parsed.report.call_graph);
        assert!(!parsed.report.children);
        assert_eq!(parsed.report.sort, Sort::Children);
        assert_eq!(parsed.report.limit, 5);
        assert_eq!(parsed.report.symbol.as_deref(), Some("Grid"));
    }

    #[test]
    fn a_symbol_is_positional_and_an_unknown_flag_is_refused() {
        let parsed = ReadArgs::parse("annotate", &args(&["Grid.step"])).expect("a symbol parses");
        assert_eq!(parsed.positional, vec!["Grid.step".to_owned()]);
        assert_eq!(
            ReadArgs::parse("report", &args(&["--nope"])).err(),
            Some(EXIT_USAGE)
        );
    }

    #[test]
    fn the_view_defaults_to_kira_and_records_when_it_was_named() {
        let parsed = ReadArgs::parse("report", &args(&[])).expect("no flags parse");
        assert_eq!(parsed.view, View::Kira);
        assert!(!parsed.view_explicit);
    }
}
