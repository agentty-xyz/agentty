//! Pure types describing the QE check engine inputs and results.
//!
//! Every recommendation in the registry maps to one [`QeCheckSpec`]. Running
//! the engine against a [`crate::infra::qe::QeProbe`] produces a
//! [`QeReport`] of [`QeResult`] entries, one per registered check.

use std::path::Path;

use crate::infra::qe::QeProbe;

/// Outcome of running one quality-enforcement check.
///
/// The engine intentionally has no `Fail` variant: every check is a
/// recommendation, never a hard gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QeOutcome {
    /// The recommendation is satisfied by the audited worktree.
    Pass,
    /// The recommendation is not satisfied; the user is advised to act.
    Warn,
}

/// Function signature shared by every recommendation acceptance closure in
/// the registry.
///
/// Receives a borrowed probe trait object and the worktree root path. Returns
/// `true` when the recommendation is satisfied.
pub type QeAcceptance = fn(probe: &dyn QeProbe, worktree: &Path) -> bool;

/// Static specification for one recommendation in the QE registry.
///
/// Entries are stored as `&'static [QeCheckSpec]` so adding or removing
/// recommendations is a single-source registry edit.
#[derive(Debug)]
pub struct QeCheckSpec {
    /// Stable machine-readable identifier (e.g. `root_agents_md`). Surfaced
    /// in the rendered report so users can reference checks by id when
    /// asking for help.
    pub id: &'static str,
    /// Human-readable headline that names what the check verifies.
    pub title: &'static str,
    /// One-sentence explanation of why the recommendation matters for
    /// agentic development. Shown in the report's `Why it matters` column.
    pub rationale: &'static str,
    /// One-sentence description of the action that satisfies the check.
    /// Reused inside the report's three-path footer prompts.
    pub remediation: &'static str,
    /// Pure acceptance function evaluated against the probe.
    pub acceptance: QeAcceptance,
}

/// Result of running one [`QeCheckSpec`] against a probe.
#[derive(Debug)]
pub struct QeResult {
    /// Borrow of the static spec the engine evaluated.
    pub spec: &'static QeCheckSpec,
    /// Outcome decided by `spec.acceptance`.
    pub outcome: QeOutcome,
}

/// Aggregated report produced by running every registered check.
#[derive(Debug)]
pub struct QeReport {
    /// Human-readable project name shown in the report header.
    pub project_name: String,
    /// One [`QeResult`] per check, ordered to match the registry.
    pub results: Vec<QeResult>,
}

impl QeReport {
    /// Returns the number of checks that produced [`QeOutcome::Warn`].
    pub fn warn_count(&self) -> usize {
        self.results
            .iter()
            .filter(|result| result.outcome == QeOutcome::Warn)
            .count()
    }

    /// Returns whether every check passed.
    pub fn is_healthy(&self) -> bool {
        self.warn_count() == 0
    }
}
