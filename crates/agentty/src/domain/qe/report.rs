//! Markdown rendering for the [`QeReport`] surfaced inside session output.
//!
//! Healthy reports collapse to a one-line confirmation; reports with at
//! least one [`super::types::QeOutcome::Warn`] include the recommendations
//! table plus the three-path footer that gives users copy-paste prompts for
//! acting on the findings.

use std::fmt::Write;

use super::types::{QeOutcome, QeReport, QeResult};

/// Renders `report` as a markdown block ready to append to a session
/// transcript.
pub fn render(report: &QeReport) -> String {
    let mut out = String::new();
    out.push_str("## /qe:check\n\n");

    if report.is_healthy() {
        let total = report.results.len();
        let _ = writeln!(
            out,
            "**Verdict:** Healthy — all {total} checks passed.\n**Project:** {}",
            report.project_name,
        );

        return out;
    }

    let _ = writeln!(
        out,
        "**Verdict:** Has {warn_count} recommendations\n**Project:** {project}\n",
        warn_count = report.warn_count(),
        project = report.project_name,
    );

    out.push_str("### Recommendations\n\n");
    out.push_str("| # | Check | Title | Why it matters |\n");
    out.push_str("|---|-------|-------|----------------|\n");

    let warned: Vec<&QeResult> = report
        .results
        .iter()
        .filter(|result| result.outcome == QeOutcome::Warn)
        .collect();
    for (index, result) in warned.iter().enumerate() {
        let _ = writeln!(
            out,
            "| {} | `{}` | {} | {} |",
            index + 1,
            result.spec.id,
            result.spec.title,
            result.spec.rationale,
        );
    }

    out.push_str("\n### How to act on this\n\n");
    out.push_str("Pick whichever fits your situation:\n\n");

    out.push_str("#### A. Recommended — roadmap-driven (multi-session)\n\n");
    out.push_str(
        "Draft a roadmap entry per recommendation, then work through them one at a time in their \
         own sessions. This teaches the agentty workflow as you harden the repo.\n\n",
    );
    out.push_str("Copy this prompt into a new session:\n\n```\n");
    out.push_str(
        "Update docs/plan/roadmap.md by seeding one regular roadmap task per recommendation \
         listed below into the existing `## Ready Now` and `## Queued Next` sections (do not add \
         any new top-level section). Tag every seeded task title with the `qe-check` stream so it \
         is easy to filter later.\n\nRecommendations to seed:\n",
    );
    for result in &warned {
        let _ = writeln!(
            out,
            "- {id}: {remediation}",
            id = result.spec.id,
            remediation = result.spec.remediation,
        );
    }
    out.push_str(
        "\nFollow the implementation-plan skill conventions for `Ready Now` and `Queued Next` \
         entries. Do not implement the recommendations themselves in this session — only seed the \
         roadmap.\n```\n\n",
    );

    out.push_str("#### B. Pick one (single-session focused fix)\n\n");
    out.push_str(
        "For a single recommendation you want to handle now, paste the prompt below, replacing \
         `<id>` with the check id from the table above:\n\n```\nAddress the /qe:check \
         recommendation `<id>`. Acceptance: <copy from remediation>. Touch only files needed to \
         satisfy this check; defer all other recommendations.\n```\n\n",
    );

    out.push_str("#### C. Implement all (single-session bulk fix)\n\n");
    out.push_str(
        "For a small repo where one session can absorb every recommendation:\n\n```\nAddress \
         every /qe:check recommendation listed below. Each must satisfy its acceptance criterion. \
         Land them as a single coherent commit.\n\n",
    );
    for (index, result) in warned.iter().enumerate() {
        let _ = writeln!(
            out,
            "{position}. {id} — {remediation}",
            position = index + 1,
            id = result.spec.id,
            remediation = result.spec.remediation,
        );
    }
    out.push_str("```\n");

    out
}

#[cfg(test)]
mod tests {
    use super::super::types::{QeCheckSpec, QeOutcome, QeResult};
    use super::*;
    use crate::infra::qe::QeProbe;

    fn dummy_acceptance(_probe: &dyn QeProbe, _worktree: &std::path::Path) -> bool {
        true
    }

    static SPEC_ALPHA: QeCheckSpec = QeCheckSpec {
        id: "alpha",
        title: "Alpha title",
        rationale: "Alpha rationale.",
        remediation: "Alpha remediation.",
        acceptance: dummy_acceptance,
    };

    static SPEC_BETA: QeCheckSpec = QeCheckSpec {
        id: "beta",
        title: "Beta title",
        rationale: "Beta rationale.",
        remediation: "Beta remediation.",
        acceptance: dummy_acceptance,
    };

    /// Verifies the healthy short form names the project and total count.
    #[test]
    fn test_render_returns_short_form_when_report_is_healthy() {
        // Arrange
        let report = QeReport {
            project_name: "agentty".to_string(),
            results: vec![
                QeResult {
                    spec: &SPEC_ALPHA,
                    outcome: QeOutcome::Pass,
                },
                QeResult {
                    spec: &SPEC_BETA,
                    outcome: QeOutcome::Pass,
                },
            ],
        };

        // Act
        let rendered = render(&report);

        // Assert
        assert!(rendered.starts_with("## /qe:check\n\n"));
        assert!(rendered.contains("**Verdict:** Healthy — all 2 checks passed."));
        assert!(rendered.contains("**Project:** agentty"));
        assert!(!rendered.contains("### Recommendations"));
    }

    /// Verifies the warn form includes verdict, project, recommendations
    /// table, and the three-path footer with each warn entry.
    #[test]
    fn test_render_returns_full_form_when_recommendations_exist() {
        // Arrange
        let report = QeReport {
            project_name: "fresh-repo".to_string(),
            results: vec![
                QeResult {
                    spec: &SPEC_ALPHA,
                    outcome: QeOutcome::Pass,
                },
                QeResult {
                    spec: &SPEC_BETA,
                    outcome: QeOutcome::Warn,
                },
            ],
        };

        // Act
        let rendered = render(&report);

        // Assert
        assert!(rendered.contains("**Verdict:** Has 1 recommendations"));
        assert!(rendered.contains("**Project:** fresh-repo"));
        assert!(rendered.contains("### Recommendations"));
        assert!(rendered.contains("| 1 | `beta` | Beta title | Beta rationale. |"));
        assert!(!rendered.contains("`alpha`"));
        assert!(rendered.contains("#### A. Recommended — roadmap-driven (multi-session)"));
        assert!(rendered.contains("#### B. Pick one (single-session focused fix)"));
        assert!(rendered.contains("#### C. Implement all (single-session bulk fix)"));
        assert!(rendered.contains("- beta: Beta remediation."));
        assert!(rendered.contains("1. beta — Beta remediation."));
    }
}
