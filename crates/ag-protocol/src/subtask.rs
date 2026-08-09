//! Orchestrator subtask model shared across protocol, coordinator, and UI
//! code.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Execution behavior requested for one orchestration subtask.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SubtaskKind {
    /// Produces repository changes for later verification and integration.
    #[default]
    Implementation,
    /// Inspects the repository without retaining any worktree changes and
    /// returns a report to the controller.
    Research,
}

/// One proposed child session in an orchestrator decomposition plan.
///
/// Each subtask is executed unattended by its own child session in its own
/// worktree, branched from the same base branch as its siblings. Children never
/// coordinate with each other while running, so each prompt must be
/// self-contained. `touched_areas` provides best-effort planning context rather
/// than an exclusive ownership boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[schemars(
    title = "SubtaskItem",
    description = "One proposed child session in an orchestrator decomposition plan. Each subtask \
                   runs unattended in its own worktree branched from the same base branch, so it \
                   must be completable without coordinating with its siblings."
)]
pub struct SubtaskItem {
    /// Observable conditions the worker must satisfy before the controller
    /// may verify this task as complete.
    #[serde(default)]
    #[schemars(
        title = "acceptance_criteria",
        description = "Concrete, testable acceptance criteria for this subtask. The approval UI \
                       shows these criteria and the controller verifies the finished child \
                       against the same list."
    )]
    pub acceptance_criteria: Vec<String>,
    /// Whether this task implements repository changes or only reports
    /// read-only findings.
    #[serde(default)]
    #[schemars(
        title = "kind",
        description = "Execution behavior for this subtask. `implementation` (the default) may \
                       change repository files and proceeds to integration. `research` runs as a \
                       temporary read-only child, returns a report, and is never integrated."
    )]
    pub kind: SubtaskKind,
    /// Complete standalone prompt handed to the child session.
    #[schemars(
        title = "prompt",
        description = "Complete standalone prompt for the child session. The child sees only this \
                       prompt and the repository, so restate every constraint it needs instead of \
                       referring back to the plan or to sibling subtasks."
    )]
    pub prompt: String,
    /// Stable identifier for this subtask within one orchestration.
    #[schemars(
        title = "task_key",
        description = "Short stable `kebab-case` identifier for this subtask, unique within the \
                       plan. Reuse the exact same key when re-proposing a subtask so a retry \
                       replaces the previous attempt instead of creating a duplicate."
    )]
    pub task_key: String,
    /// Short human-readable subtask title.
    #[schemars(
        title = "title",
        description = "Short human-readable title describing what this subtask delivers."
    )]
    pub title: String,
    /// Best-effort repository-relative paths or directories this subtask is
    /// expected to change.
    #[serde(default)]
    #[schemars(
        title = "touched_areas",
        description = "Best-effort repository-relative file or directory paths this subtask is \
                       expected to modify. Wildcard patterns are not supported. Paths may overlap \
                       between subtasks and workers may modify additional files when needed to \
                       satisfy the task. Defaults to an empty list when omitted."
    )]
    pub touched_areas: Vec<String>,
}
