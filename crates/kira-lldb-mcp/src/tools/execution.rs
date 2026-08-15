//! Resuming, stepping, pausing, and running a target to its end.
//!
//! Stepping means two different things here. A native frame is stepped by the
//! debugger, which knows the line table. A bytecode frame has no line table and
//! no machine instructions of its own: the VM calls one probe per interpreted
//! instruction, so a step is a resume to the next probe stop, and stepping over
//! a call is resuming until the Kira call depth comes back down. That is why
//! the stopped VM state is read between resumes rather than a DAP step being
//! sent and hoped over.

use std::time::Duration;

use kira_debug::{DEFAULT_TIMEOUT, TargetState};
use serde_json::{Value, json};

use super::{descriptor, enum_field, session_field, session_property, uint_field};
use crate::registry::Sessions;
use crate::report::stop_report;
use crate::session::Session;

/// How long a resume may take before the caller is told it is still running.
const RESUME_TIMEOUT: Duration = Duration::from_secs(120);
/// How many VM stops a single step may pass through before it gives up.
///
/// Stepping over a Kira call resumes once per interpreted instruction inside
/// it, so a call that runs for millions of instructions would otherwise hold
/// the session forever with nothing to show. The limit is reported when it is
/// reached, so a caller can raise it rather than wonder where the step went.
const STEP_BUDGET: u64 = 20_000;

/// The ways a step can move.
const MODES: [&str; 4] = ["into", "over", "out", "instruction"];

/// The execution tools.
pub fn descriptors() -> Vec<Value> {
    vec![
        descriptor(
            "kira_lldb_continue",
            "Resume the target until the next breakpoint. With `count`, resume that \
             many times. With `until`, resume until a Kira `function` or \
             `function:instruction` is reached.",
            json!({
                "session": session_property(),
                "count": {
                    "type": "integer",
                    "description": "How many times to resume. Defaults to 1.",
                },
                "until": {
                    "type": "string",
                    "description": "Resume until this Kira function or function:instruction.",
                },
            }),
            &[],
        ),
        descriptor(
            "kira_lldb_step",
            "Step the stopped thread. `into` enters calls, `over` runs them to \
             completion, `out` finishes the current function, and `instruction` \
             advances one bytecode or machine instruction.",
            json!({
                "session": session_property(),
                "mode": {
                    "type": "string",
                    "enum": MODES,
                    "description": "How to step. Defaults to over.",
                },
                "count": {
                    "type": "integer",
                    "description": "How many steps to take. Defaults to 1.",
                },
                "budget": {
                    "type": "integer",
                    "description": "How many VM stops one step may pass through.",
                },
            }),
            &[],
        ),
        descriptor(
            "kira_lldb_pause",
            "Stop a running target where it is.",
            json!({ "session": session_property() }),
            &[],
        ),
        descriptor(
            "kira_lldb_finish",
            "Let the target run to completion and report its exit status and output.",
            json!({ "session": session_property() }),
            &[],
        ),
    ]
}

/// Resumes the target.
pub fn resume(sessions: &mut Sessions, arguments: &Value) -> Result<Value, String> {
    let count = uint_field(arguments, "count", 1)?.max(1);
    let until = arguments["until"].as_str().map(str::to_owned);
    let session = sessions.select(session_field(arguments))?;
    match until {
        Some(until) => resume_until(session, &until),
        None => {
            for _ in 0..count {
                if session.resume(RESUME_TIMEOUT)?.is_none() {
                    break;
                }
            }
            Ok(stop_report(session))
        }
    }
}

/// Resumes until a Kira location is reached, or the target ends.
///
/// The location is installed as a temporary probe condition rather than
/// stepped to, so the interpreter runs at full speed between here and there.
fn resume_until(session: &mut Session, until: &str) -> Result<Value, String> {
    let placement = super::breakpoints::kira_placement(session, until)?;
    let breakpoint = session.add_breakpoint(placement, None)?;
    let outcome = session.resume(RESUME_TIMEOUT);
    // The temporary breakpoint goes whether the resume arrived or not, so a
    // session is never left stopping at a location the caller asked about once.
    let removal = session.remove_breakpoints(Some(&[breakpoint.id]));
    outcome?;
    removal?;
    let mut report = stop_report(session);
    report["until"] = json!(until);
    Ok(report)
}

/// Steps the stopped thread.
pub fn step(sessions: &mut Sessions, arguments: &Value) -> Result<Value, String> {
    let mode = enum_field(arguments, "mode", &MODES, "over")?.to_owned();
    let count = uint_field(arguments, "count", 1)?.max(1);
    let budget = uint_field(arguments, "budget", STEP_BUDGET)?.max(1);
    let session = sessions.select(session_field(arguments))?;
    let mut passed = 0;
    let mut exhausted = false;
    session.begin_step()?;
    let mut outcome = Ok(());
    for _ in 0..count {
        match session.target.probe.is_some() {
            true => match step_bytecode(session, &mode, budget) {
                Ok(taken) => {
                    passed += taken;
                    if taken >= budget {
                        exhausted = true;
                        break;
                    }
                }
                Err(error) => {
                    outcome = Err(error);
                    break;
                }
            },
            false => {
                if let Err(error) = step_native(session, &mode) {
                    outcome = Err(error);
                    break;
                }
            }
        }
        if !session.state().is_alive() {
            break;
        }
    }
    // The conditions come back whether the step arrived or not: a session left
    // with a free-running probe would stop at every interpreted instruction
    // for the rest of its life.
    let restored = session.end_step();
    outcome?;
    restored?;
    let mut report = stop_report(session);
    report["mode"] = json!(mode);
    if session.target.probe.is_some() {
        report["vm_stops"] = json!(passed);
        if exhausted {
            report["budget_exhausted"] = json!(budget);
        }
    }
    Ok(report)
}

/// Steps one bytecode instruction, call, or frame.
///
/// Returns how many VM stops were passed through, which is what tells the
/// caller whether a step that ended without arriving ran out of budget.
fn step_bytecode(session: &mut Session, mode: &str, budget: u64) -> Result<u64, String> {
    let start = session
        .vm_stop()?
        .ok_or_else(|| "the VM has not reached an instruction stop yet".to_owned())?;
    let mut passed = 0;
    while passed < budget {
        if session.resume(RESUME_TIMEOUT)?.is_none() {
            return Ok(passed);
        }
        passed += 1;
        let Some(current) = session.vm_stop()? else {
            return Ok(passed);
        };
        if arrived(mode, start.call_depth, current.call_depth) {
            return Ok(passed);
        }
    }
    Ok(passed)
}

/// Whether a bytecode step that began at `start` depth has finished.
///
/// `into` and `instruction` stop at the very next interpreted instruction,
/// wherever it is. `over` accepts any depth at or below where it started, so a
/// call that returns lands on the instruction after it. `out` requires a
/// strictly shallower frame, which is what makes it leave the function rather
/// than stop at its next instruction.
fn arrived(mode: &str, start: u32, current: u32) -> bool {
    match mode {
        "over" => current <= start,
        "out" => current < start,
        _ => true,
    }
}

/// The DAP request and granularity one native step mode uses.
fn native_step(mode: &str) -> (&'static str, &'static str) {
    match mode {
        "into" => ("stepIn", "statement"),
        "out" => ("stepOut", "statement"),
        "instruction" => ("next", "instruction"),
        _ => ("next", "statement"),
    }
}

/// Steps a native frame with the debugger's own line table.
fn step_native(session: &mut Session, mode: &str) -> Result<(), String> {
    let thread_id = session
        .client()
        .stopped_thread()
        .map_err(|error| error.to_string())?;
    let (command, granularity) = native_step(mode);
    session
        .client()
        .request(
            command,
            json!({ "threadId": thread_id, "granularity": granularity }),
            DEFAULT_TIMEOUT,
        )
        .map_err(|error| error.to_string())?;
    session.client().mark_running();
    session.await_stop(RESUME_TIMEOUT)?;
    Ok(())
}

/// Stops a running target.
pub fn pause(sessions: &mut Sessions, arguments: &Value) -> Result<Value, String> {
    let session = sessions.select(session_field(arguments))?;
    let thread = match session.state() {
        TargetState::Stopped(stop) => stop.thread_id,
        _ => 1,
    };
    session
        .client()
        .request("pause", json!({ "threadId": thread }), DEFAULT_TIMEOUT)
        .map_err(|error| error.to_string())?;
    session.await_stop(RESUME_TIMEOUT)?;
    Ok(stop_report(session))
}

/// Runs the target to completion.
///
/// Every breakpoint goes first, which on a bytecode target also removes the VM
/// probe: left installed it would stop the program once per interpreted
/// instruction, and "run to the end" would take millions of resumes to get
/// somewhere the program reaches on its own in an instant.
pub fn finish(sessions: &mut Sessions, arguments: &Value) -> Result<Value, String> {
    let session = sessions.select(session_field(arguments))?;
    session.remove_breakpoints(None)?;
    session.set_vm_stops(false)?;
    while session.state().is_alive() {
        if session.resume(RESUME_TIMEOUT)?.is_none() {
            break;
        }
    }
    Ok(stop_report(session))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stepping_refuses_a_mode_it_cannot_perform() {
        let mut sessions = Sessions::default();
        assert!(step(&mut sessions, &json!({ "mode": "backwards" })).is_err());
    }

    /// A count of zero would answer without moving, which reads as a step that
    /// did nothing rather than a request that meant nothing.
    #[test]
    fn a_count_below_one_still_takes_one_step() {
        assert_eq!(
            uint_field(&json!({ "count": 0 }), "count", 1).map(|count| count.max(1)),
            Ok(1)
        );
    }

    /// A caller may raise or lower the budget, and a zero would make a step
    /// return without moving.
    #[test]
    fn the_step_budget_defaults_and_is_overridable_but_never_zero() {
        assert_eq!(
            uint_field(&json!({}), "budget", STEP_BUDGET),
            Ok(STEP_BUDGET)
        );
        assert_eq!(
            uint_field(&json!({ "budget": 5 }), "budget", STEP_BUDGET),
            Ok(5)
        );
        assert_eq!(
            uint_field(&json!({ "budget": 0 }), "budget", STEP_BUDGET).map(|budget| budget.max(1)),
            Ok(1)
        );
    }

    /// Stepping over a call must not stop inside it, and must stop as soon as
    /// it returns.
    #[test]
    fn stepping_over_waits_for_the_call_it_entered_to_return() {
        assert!(!arrived("over", 1, 2), "inside the call");
        assert!(arrived("over", 1, 1), "back in the caller");
        assert!(arrived("over", 2, 1), "the caller itself returned");
    }

    /// Stepping out must leave the frame it started in, so the same depth is
    /// not far enough.
    #[test]
    fn stepping_out_needs_a_shallower_frame_than_it_started_in() {
        assert!(!arrived("out", 2, 2), "still in the frame");
        assert!(!arrived("out", 2, 3), "deeper still");
        assert!(arrived("out", 2, 1), "left the frame");
    }

    #[test]
    fn stepping_into_or_by_instruction_stops_at_the_next_one_wherever_it_is() {
        for mode in ["into", "instruction"] {
            assert!(
                arrived(mode, 1, 9),
                "`{mode}` stops at the next instruction"
            );
            assert!(
                arrived(mode, 9, 1),
                "`{mode}` stops at the next instruction"
            );
        }
    }

    #[test]
    fn each_native_mode_maps_to_the_request_that_performs_it() {
        assert_eq!(native_step("into"), ("stepIn", "statement"));
        assert_eq!(native_step("out"), ("stepOut", "statement"));
        assert_eq!(native_step("over"), ("next", "statement"));
        assert_eq!(native_step("instruction"), ("next", "instruction"));
    }

    #[test]
    fn every_advertised_mode_has_a_native_mapping_and_an_arrival_rule() {
        for mode in MODES {
            let (command, _) = native_step(mode);
            assert!(
                ["next", "stepIn", "stepOut"].contains(&command),
                "`{mode}` maps to `{command}`"
            );
            // An arrival rule that never fires would hang a bytecode step.
            assert!(
                arrived(mode, 1, 0),
                "`{mode}` must arrive once the frame is left"
            );
        }
    }
}
