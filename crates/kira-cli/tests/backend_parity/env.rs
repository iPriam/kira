//! Parity for reading the process environment.
//!
//! The VM answers these itself and native code calls `kira_rt_env_*`, so this is
//! the test that keeps one implementation honest against the other. Both read
//! the same variables from the same process — the test harness runs each engine
//! as a child, so what this asserts is that neither invents an answer.

use crate::assert_parity;

/// A variable no machine sets, so the unset answers are the same everywhere
/// this runs — including CI, where a real variable's value is not knowable.
#[test]
fn an_unset_variable_reads_the_same_on_both_engines() {
    let output = assert_parity(
        r#"
@Main
function main() {
    let absent = "KIRA_PARITY_ENV_DEFINITELY_UNSET"
    print(envIsSet(absent))
    print(envText(absent))
    print(envText(absent).count)
    return
}
"#,
    );
    assert_eq!(output, "false\n\n0\n");
}

/// `PATH` is set on every host either engine runs on, and both must agree that
/// it is — without printing its value, which differs per machine.
#[test]
fn a_variable_the_host_sets_is_seen_by_both_engines() {
    let output = assert_parity(
        r#"
@Main
function main() {
    print(envIsSet("PATH"))
    print(envText("PATH").count > 0)
    return
}
"#,
    );
    assert_eq!(output, "true\ntrue\n");
}

/// Foundation's wrappers, including the number parse, which is Kira code rather
/// than an intrinsic and so has to agree across engines on its own.
#[test]
fn foundations_wrappers_agree_across_engines() {
    let output = assert_parity(
        r#"
import Foundation

@Main
function main() {
    let absent = "KIRA_PARITY_ENV_DEFINITELY_UNSET"
    print(environmentIsSet(absent))
    print(environmentValue(absent))
    // Unset, so every one of these is the fallback.
    print(environmentInt(absent, 7))
    return
}
"#,
    );
    assert_eq!(output, "false\n\n7\n");
}
