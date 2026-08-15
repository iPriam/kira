//! The verbs that read a recording and run nothing.
//!
//! Each one loads a trace, picks a view, and prints. The only judgement they
//! make is which view to print when the caller did not say: the Kira view when
//! it has samples, and the machine view when it does not — because a native
//! recording has no separate Kira view and a caller asking "where did the time
//! go" should not have to know that.

use std::path::Path;

use kira_profile::model::{Profile, View};
use kira_profile::render::annotate::{NoSiteText, SiteText};
use kira_profile::render::{
    annotate, diff as diff_render, folded, report as report_render, script as script_render,
    stat as stat_render,
};
use kira_profile::trace::Trace;

use super::ReadArgs;
use crate::pipeline::{EXIT_FAILURE, EXIT_USAGE};
use crate::progress::err;

/// Runs `kira profile report`.
pub(super) fn report(args: &[String]) -> i32 {
    let parsed = match ReadArgs::parse("report", args) {
        Ok(parsed) => parsed,
        Err(code) => return code,
    };
    let Some(trace) = load("report", &parsed) else {
        return EXIT_FAILURE;
    };
    let Some(profile) = pick("report", &trace, &parsed) else {
        return EXIT_FAILURE;
    };
    if parsed.folded {
        return super::emit(&folded::render(profile, &parsed.report));
    }
    super::emit(&report_render::render(&trace.meta, profile, &parsed.report))
}

/// Runs `kira profile annotate`.
pub(super) fn annotate(args: &[String]) -> i32 {
    let parsed = match ReadArgs::parse("annotate", args) {
        Ok(parsed) => parsed,
        Err(code) => return code,
    };
    let Some(symbol) = parsed.positional.first().cloned() else {
        err!("kira profile annotate: name the symbol to annotate");
        return EXIT_USAGE;
    };
    let Some(trace) = load("annotate", &parsed) else {
        return EXIT_FAILURE;
    };
    let Some(profile) = pick("annotate", &trace, &parsed) else {
        return EXIT_FAILURE;
    };
    let disassembly = trace
        .meta
        .source
        .as_deref()
        .and_then(super::instructions::disassemble);
    let text: &dyn SiteText = match &disassembly {
        Some(disassembly) => disassembly,
        None => &NoSiteText,
    };
    super::emit(&annotate::render(profile, &symbol, &parsed.report, text))
}

/// Runs `kira profile script`.
pub(super) fn script(args: &[String]) -> i32 {
    let parsed = match ReadArgs::parse("script", args) {
        Ok(parsed) => parsed,
        Err(code) => return code,
    };
    let Some(trace) = load("script", &parsed) else {
        return EXIT_FAILURE;
    };
    let Some(profile) = pick("script", &trace, &parsed) else {
        return EXIT_FAILURE;
    };
    super::emit(&script_render::render(&trace.meta, profile, &parsed.report))
}

/// Runs `kira profile stat`.
pub(super) fn stat(args: &[String]) -> i32 {
    let parsed = match ReadArgs::parse("stat", args) {
        Ok(parsed) => parsed,
        Err(code) => return code,
    };
    let Some(trace) = load("stat", &parsed) else {
        return EXIT_FAILURE;
    };
    super::emit(&stat_render::render(&trace))
}

/// Runs `kira profile diff`.
pub(super) fn diff(args: &[String]) -> i32 {
    let parsed = match ReadArgs::parse("diff", args) {
        Ok(parsed) => parsed,
        Err(code) => return code,
    };
    let (Some(baseline), Some(current)) = (parsed.positional.first(), parsed.positional.get(1))
    else {
        err!("kira profile diff: name the baseline recording and the current one");
        return EXIT_USAGE;
    };
    let (Some(baseline), Some(current)) = (
        read("diff", Path::new(baseline)),
        read("diff", Path::new(current)),
    ) else {
        return EXIT_FAILURE;
    };
    let (Some(left), Some(right)) = (
        view("diff", &baseline, parsed.view),
        view("diff", &current, parsed.view),
    ) else {
        return EXIT_FAILURE;
    };
    super::emit(&diff_render::render(left, right, &parsed.report))
}

/// Loads the recording a reading verb was pointed at.
fn load(verb: &str, parsed: &ReadArgs) -> Option<Trace> {
    if !parsed.input.exists() {
        err!(
            "kira profile {verb}: no recording at `{}`\n\
             note: `kira profile record` writes one",
            parsed.input.display()
        );
        return None;
    }
    read(verb, &parsed.input)
}

fn read(verb: &str, path: &Path) -> Option<Trace> {
    match Trace::load(path) {
        Ok(trace) => Some(trace),
        Err(error) => {
            err!("kira profile {verb}: {error}");
            None
        }
    }
}

/// The view to render, falling back when the one asked for is not there.
fn pick<'a>(verb: &str, trace: &'a Trace, parsed: &ReadArgs) -> Option<&'a Profile> {
    let wanted = trace
        .view(parsed.view)
        .filter(|profile| !profile.samples.is_empty());
    if let Some(profile) = wanted {
        return Some(profile);
    }
    if parsed.view_explicit {
        return match trace.view(parsed.view) {
            // Present but empty: render it, so the report says what happened
            // rather than substituting a view nobody asked for.
            Some(profile) => Some(profile),
            None => {
                err!(
                    "kira profile {verb}: this recording has no {} view",
                    parsed.view.label()
                );
                None
            }
        };
    }
    let other = match parsed.view {
        View::Kira => View::Machine,
        View::Machine => View::Kira,
    };
    trace
        .view(other)
        .or_else(|| trace.profiles.first())
        .or_else(|| {
            err!("kira profile {verb}: this recording has no samples at all");
            None
        })
}

fn view<'a>(verb: &str, trace: &'a Trace, view: View) -> Option<&'a Profile> {
    match trace.view(view) {
        Some(profile) => Some(profile),
        None => {
            err!(
                "kira profile {verb}: a recording has no {} view",
                view.label()
            );
            None
        }
    }
}
