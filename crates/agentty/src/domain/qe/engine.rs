//! Pure execution engine for the QE recommendation registry.
//!
//! [`run()`] iterates [`super::registry::REGISTRY`] once, evaluates every
//! acceptance closure against the supplied probe and worktree path, and
//! returns the aggregated [`QeReport`].

use std::path::Path;

use super::registry::REGISTRY;
use super::types::{QeOutcome, QeReport, QeResult};
use crate::infra::qe::QeProbe;

/// Runs every recommendation in the registry against `probe` rooted at
/// `worktree`, capturing the supplied `project_name` in the resulting
/// [`QeReport`] header field.
pub fn run(probe: &dyn QeProbe, worktree: &Path, project_name: String) -> QeReport {
    let results = REGISTRY
        .iter()
        .map(|spec| {
            let outcome = if (spec.acceptance)(probe, worktree) {
                QeOutcome::Pass
            } else {
                QeOutcome::Warn
            };

            QeResult { spec, outcome }
        })
        .collect();

    QeReport {
        project_name,
        results,
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use mockall::predicate::eq;

    use super::*;
    use crate::infra::qe::MockQeProbe;

    const WORKTREE: &str = "/tmp/test-worktree";

    fn worktree_path() -> PathBuf {
        PathBuf::from(WORKTREE)
    }

    fn worktree() -> &'static Path {
        Path::new(WORKTREE)
    }

    /// Builds a [`MockQeProbe`] that satisfies every recommendation in the
    /// registry. Tests override individual responses to drive specific
    /// recommendations into [`QeOutcome::Warn`].
    fn fully_passing_probe() -> MockQeProbe {
        let mut probe = MockQeProbe::new();
        probe
            .expect_is_file()
            .with(eq(worktree_path().join("AGENTS.md")))
            .returning(|_| true);
        probe
            .expect_read_link()
            .with(eq(worktree_path().join("CLAUDE.md")))
            .returning(|_| Some(PathBuf::from("AGENTS.md")));
        probe
            .expect_read_link()
            .with(eq(worktree_path().join("GEMINI.md")))
            .returning(|_| Some(PathBuf::from("AGENTS.md")));
        probe
            .expect_read_to_string()
            .with(eq(worktree_path().join(".gitignore")))
            .returning(|_| Some(".agentty/\n".to_string()));
        probe
            .expect_is_file()
            .with(eq(worktree_path().join("README.md")))
            .returning(|_| true);
        probe
            .expect_is_file()
            .with(eq(worktree_path().join("docs/plan/roadmap.md")))
            .returning(|_| true);
        probe
            .expect_read_to_string()
            .with(eq(worktree_path().join("docs/plan/roadmap.md")))
            .returning(|_| Some("## Ready Now\n## Queued Next\n## Parked\n".to_string()));
        probe
            .expect_is_file()
            .with(eq(worktree_path().join(".pre-commit-config.yaml")))
            .returning(|_| true);

        probe
    }

    /// Verifies `run()` returns a healthy report when every recommendation
    /// passes.
    #[test]
    fn test_run_returns_healthy_report_when_every_check_passes() {
        // Arrange
        let probe = fully_passing_probe();

        // Act
        let report = run(&probe, worktree(), "agentty".to_string());

        // Assert
        assert_eq!(report.project_name, "agentty");
        assert_eq!(report.results.len(), REGISTRY.len());
        assert!(report.is_healthy());
        assert_eq!(report.warn_count(), 0);
    }

    /// Verifies missing root `AGENTS.md` warns and leaves other checks
    /// passing.
    #[test]
    fn test_run_warns_when_root_agents_md_missing() {
        // Arrange
        let mut probe = MockQeProbe::new();
        probe
            .expect_is_file()
            .with(eq(worktree_path().join("AGENTS.md")))
            .returning(|_| false);
        probe
            .expect_read_link()
            .with(eq(worktree_path().join("CLAUDE.md")))
            .returning(|_| Some(PathBuf::from("AGENTS.md")));
        probe
            .expect_read_link()
            .with(eq(worktree_path().join("GEMINI.md")))
            .returning(|_| Some(PathBuf::from("AGENTS.md")));
        probe
            .expect_read_to_string()
            .with(eq(worktree_path().join(".gitignore")))
            .returning(|_| Some(".agentty/\n".to_string()));
        probe
            .expect_is_file()
            .with(eq(worktree_path().join("README.md")))
            .returning(|_| true);
        probe
            .expect_is_file()
            .with(eq(worktree_path().join("docs/plan/roadmap.md")))
            .returning(|_| true);
        probe
            .expect_read_to_string()
            .with(eq(worktree_path().join("docs/plan/roadmap.md")))
            .returning(|_| Some("## Ready Now\n## Queued Next\n## Parked\n".to_string()));
        probe
            .expect_is_file()
            .with(eq(worktree_path().join(".pre-commit-config.yaml")))
            .returning(|_| true);

        // Act
        let report = run(&probe, worktree(), "fresh-repo".to_string());

        // Assert
        assert_eq!(report.warn_count(), 1);
        let warned = report
            .results
            .iter()
            .find(|result| result.outcome == QeOutcome::Warn)
            .expect("one warn entry");
        assert_eq!(warned.spec.id, "root_agents_md");
    }

    /// Verifies a CLAUDE.md symlink pointing at the wrong target warns the
    /// `agents_md_symlinks` recommendation.
    #[test]
    fn test_run_warns_when_claude_md_symlink_points_elsewhere() {
        // Arrange
        let mut probe = MockQeProbe::new();
        probe
            .expect_is_file()
            .with(eq(worktree_path().join("AGENTS.md")))
            .returning(|_| true);
        probe
            .expect_read_link()
            .with(eq(worktree_path().join("CLAUDE.md")))
            .returning(|_| Some(PathBuf::from("README.md")));
        probe
            .expect_read_link()
            .with(eq(worktree_path().join("GEMINI.md")))
            .returning(|_| Some(PathBuf::from("AGENTS.md")));
        probe
            .expect_read_to_string()
            .with(eq(worktree_path().join(".gitignore")))
            .returning(|_| Some(".agentty/\n".to_string()));
        probe
            .expect_is_file()
            .with(eq(worktree_path().join("README.md")))
            .returning(|_| true);
        probe
            .expect_is_file()
            .with(eq(worktree_path().join("docs/plan/roadmap.md")))
            .returning(|_| true);
        probe
            .expect_read_to_string()
            .with(eq(worktree_path().join("docs/plan/roadmap.md")))
            .returning(|_| Some("## Ready Now\n## Queued Next\n## Parked\n".to_string()));
        probe
            .expect_is_file()
            .with(eq(worktree_path().join(".pre-commit-config.yaml")))
            .returning(|_| true);

        // Act
        let report = run(&probe, worktree(), "fresh-repo".to_string());

        // Assert
        let warned_ids: Vec<&str> = report
            .results
            .iter()
            .filter(|result| result.outcome == QeOutcome::Warn)
            .map(|result| result.spec.id)
            .collect();
        assert_eq!(warned_ids, ["agents_md_symlinks"]);
    }

    /// Verifies `.gitignore` content without a `.agentty/` entry warns the
    /// related recommendation.
    #[test]
    fn test_run_warns_when_gitignore_omits_agentty() {
        // Arrange
        let mut probe = MockQeProbe::new();
        probe
            .expect_is_file()
            .with(eq(worktree_path().join("AGENTS.md")))
            .returning(|_| true);
        probe
            .expect_read_link()
            .with(eq(worktree_path().join("CLAUDE.md")))
            .returning(|_| Some(PathBuf::from("AGENTS.md")));
        probe
            .expect_read_link()
            .with(eq(worktree_path().join("GEMINI.md")))
            .returning(|_| Some(PathBuf::from("AGENTS.md")));
        probe
            .expect_read_to_string()
            .with(eq(worktree_path().join(".gitignore")))
            .returning(|_| Some("target/\nnode_modules/\n".to_string()));
        probe
            .expect_is_file()
            .with(eq(worktree_path().join("README.md")))
            .returning(|_| true);
        probe
            .expect_is_file()
            .with(eq(worktree_path().join("docs/plan/roadmap.md")))
            .returning(|_| true);
        probe
            .expect_read_to_string()
            .with(eq(worktree_path().join("docs/plan/roadmap.md")))
            .returning(|_| Some("## Ready Now\n## Queued Next\n## Parked\n".to_string()));
        probe
            .expect_is_file()
            .with(eq(worktree_path().join(".pre-commit-config.yaml")))
            .returning(|_| true);

        // Act
        let report = run(&probe, worktree(), "fresh-repo".to_string());

        // Assert
        let warned_ids: Vec<&str> = report
            .results
            .iter()
            .filter(|result| result.outcome == QeOutcome::Warn)
            .map(|result| result.spec.id)
            .collect();
        assert_eq!(warned_ids, ["gitignore_excludes_agentty"]);
    }

    /// Verifies a roadmap file missing the standard sections warns the
    /// `roadmap_present` recommendation even when the file exists.
    #[test]
    fn test_run_warns_when_roadmap_missing_standard_sections() {
        // Arrange
        let mut probe = MockQeProbe::new();
        probe
            .expect_is_file()
            .with(eq(worktree_path().join("AGENTS.md")))
            .returning(|_| true);
        probe
            .expect_read_link()
            .with(eq(worktree_path().join("CLAUDE.md")))
            .returning(|_| Some(PathBuf::from("AGENTS.md")));
        probe
            .expect_read_link()
            .with(eq(worktree_path().join("GEMINI.md")))
            .returning(|_| Some(PathBuf::from("AGENTS.md")));
        probe
            .expect_read_to_string()
            .with(eq(worktree_path().join(".gitignore")))
            .returning(|_| Some(".agentty/\n".to_string()));
        probe
            .expect_is_file()
            .with(eq(worktree_path().join("README.md")))
            .returning(|_| true);
        probe
            .expect_is_file()
            .with(eq(worktree_path().join("docs/plan/roadmap.md")))
            .returning(|_| true);
        probe
            .expect_read_to_string()
            .with(eq(worktree_path().join("docs/plan/roadmap.md")))
            .returning(|_| Some("# Some other heading\n".to_string()));
        probe
            .expect_is_file()
            .with(eq(worktree_path().join(".pre-commit-config.yaml")))
            .returning(|_| true);

        // Act
        let report = run(&probe, worktree(), "fresh-repo".to_string());

        // Assert
        let warned_ids: Vec<&str> = report
            .results
            .iter()
            .filter(|result| result.outcome == QeOutcome::Warn)
            .map(|result| result.spec.id)
            .collect();
        assert_eq!(warned_ids, ["roadmap_present"]);
    }

    /// Builds a [`MockQeProbe`] that satisfies every recommendation except
    /// `gitignore_excludes_agentty`, which is driven by `gitignore_content`.
    fn probe_with_gitignore(gitignore_content: &'static str) -> MockQeProbe {
        let mut probe = MockQeProbe::new();
        probe
            .expect_is_file()
            .with(eq(worktree_path().join("AGENTS.md")))
            .returning(|_| true);
        probe
            .expect_read_link()
            .with(eq(worktree_path().join("CLAUDE.md")))
            .returning(|_| Some(PathBuf::from("AGENTS.md")));
        probe
            .expect_read_link()
            .with(eq(worktree_path().join("GEMINI.md")))
            .returning(|_| Some(PathBuf::from("AGENTS.md")));
        probe
            .expect_read_to_string()
            .with(eq(worktree_path().join(".gitignore")))
            .returning(move |_| Some(gitignore_content.to_string()));
        probe
            .expect_is_file()
            .with(eq(worktree_path().join("README.md")))
            .returning(|_| true);
        probe
            .expect_is_file()
            .with(eq(worktree_path().join("docs/plan/roadmap.md")))
            .returning(|_| true);
        probe
            .expect_read_to_string()
            .with(eq(worktree_path().join("docs/plan/roadmap.md")))
            .returning(|_| Some("## Ready Now\n## Queued Next\n## Parked\n".to_string()));
        probe
            .expect_is_file()
            .with(eq(worktree_path().join(".pre-commit-config.yaml")))
            .returning(|_| true);

        probe
    }

    /// Verifies the gitignore check accepts the anchored `/.agentty/`
    /// variant in addition to the bare `.agentty/` form.
    #[test]
    fn test_run_passes_gitignore_when_anchored_pattern_present() {
        // Arrange
        let probe = probe_with_gitignore("/.agentty/\n");

        // Act
        let report = run(&probe, worktree(), "agentty".to_string());

        // Assert
        assert_eq!(report.warn_count(), 0);
    }

    /// Verifies a later `!.agentty/` negation cancels the prior exclusion
    /// and warns the recommendation.
    #[test]
    fn test_run_warns_when_gitignore_negation_cancels_exclusion() {
        // Arrange
        let probe = probe_with_gitignore(".agentty/\n!.agentty/\n");

        // Act
        let report = run(&probe, worktree(), "agentty".to_string());

        // Assert
        let warned_ids: Vec<&str> = report
            .results
            .iter()
            .filter(|result| result.outcome == QeOutcome::Warn)
            .map(|result| result.spec.id)
            .collect();
        assert_eq!(warned_ids, ["gitignore_excludes_agentty"]);
    }

    /// Verifies comments and blank lines are skipped without affecting the
    /// match decision.
    #[test]
    fn test_run_passes_gitignore_with_comments_and_blank_lines() {
        // Arrange
        let probe = probe_with_gitignore("# scratch dirs\n\n.agentty/\n");

        // Act
        let report = run(&probe, worktree(), "agentty".to_string());

        // Assert
        assert_eq!(report.warn_count(), 0);
    }

    /// Verifies a fresh `git init` worktree (no agentic conventions in
    /// place) warns every recommendation in the registry.
    #[test]
    fn test_run_warns_every_recommendation_for_fresh_repo() {
        // Arrange
        let mut probe = MockQeProbe::new();
        probe.expect_is_file().returning(|_| false);
        probe.expect_read_link().returning(|_| None);
        probe.expect_read_to_string().returning(|_| None);

        // Act
        let report = run(&probe, worktree(), "fresh-repo".to_string());

        // Assert
        assert_eq!(report.warn_count(), REGISTRY.len());
        assert!(!report.is_healthy());
    }
}
