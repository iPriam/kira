//! Pure landing-gate evaluation for `devflow land`.
//!
//! Gate inputs are collected from GitHub in `commands::land`; the decision
//! itself stays pure so the gate matrix is unit-testable without network.

use crate::gh_ops::CheckStatus;

/// Evaluate the landing gates: green CI, a submitted bot review, zero
/// unresolved threads.
///
/// Returns the refusal reason, or `None` when all gates pass. A forced land
/// prints the reason as a warning instead of refusing.
pub fn refusal(
    checks: &CheckStatus,
    submitted: &str,
    has_rabbit: bool,
    has_codex: bool,
    require_codex: bool,
    unresolved: u32,
) -> Option<String> {
    if !checks.green() {
        return Some(format!(
            "CI is not green ({} pending, {} failing)\n{}",
            checks.pending, checks.failing, checks.lines
        ));
    }
    if !(has_rabbit && (!require_codex || has_codex)) {
        return Some(format!(
            "required review not submitted yet (submitted: {submitted})"
        ));
    }
    if unresolved != 0 {
        return Some(format!("{unresolved} unresolved review thread(s)"));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checks(total: u32, pending: u32, failing: u32) -> CheckStatus {
        CheckStatus {
            lines: String::from("rows"),
            total,
            pending,
            failing,
        }
    }

    #[test]
    fn green_gates_pass() {
        assert_eq!(
            refusal(&checks(12, 0, 0), "coderabbitai", true, false, false, 0),
            None
        );
    }

    #[test]
    fn failing_ci_refuses() {
        let reason = refusal(&checks(12, 0, 3), "coderabbitai", true, false, false, 0)
            .expect("failing CI must refuse");
        assert!(reason.contains("CI is not green"));
    }

    #[test]
    fn pending_ci_refuses() {
        assert!(refusal(&checks(12, 2, 0), "coderabbitai", true, false, false, 0).is_some());
    }

    #[test]
    fn no_checks_refuses() {
        assert!(refusal(&checks(0, 0, 0), "coderabbitai", true, false, false, 0).is_some());
    }

    #[test]
    fn missing_review_refuses() {
        let reason = refusal(&checks(12, 0, 0), "", false, false, false, 0)
            .expect("missing review must refuse");
        assert!(reason.contains("review not submitted"));
    }

    #[test]
    fn codex_gate_requires_codex() {
        assert!(
            refusal(&checks(12, 0, 0), "coderabbitai", true, false, true, 0).is_some(),
            "demanded Codex review must refuse without one"
        );
        assert_eq!(
            refusal(&checks(12, 0, 0), "coderabbitai,codex", true, true, true, 0),
            None
        );
    }

    #[test]
    fn unresolved_threads_refuse() {
        let reason = refusal(&checks(12, 0, 0), "coderabbitai", true, false, false, 2)
            .expect("unresolved threads must refuse");
        assert!(reason.contains("2 unresolved"));
    }
}
