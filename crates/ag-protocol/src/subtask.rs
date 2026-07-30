//! Orchestrator subtask model shared across protocol, coordinator, and UI
//! code.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// One proposed child session in an orchestrator decomposition plan.
///
/// Each subtask is executed unattended by its own child session in its own
/// worktree, branched from the same base branch as its siblings. Children never
/// coordinate with each other while running, so a subtask is only well-formed
/// when its prompt is self-contained and its `touched_areas` do not overlap any
/// sibling's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[schemars(
    title = "SubtaskItem",
    description = "One proposed child session in an orchestrator decomposition plan. Each subtask \
                   runs unattended in its own worktree branched from the same base branch, so it \
                   must be completable without coordinating with its siblings."
)]
pub struct SubtaskItem {
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
    /// Literal repository-relative paths or directories this subtask expects
    /// to change.
    #[serde(default)]
    #[schemars(
        title = "touched_areas",
        description = "Literal repository-relative file or directory paths this subtask expects \
                       to modify. Wildcard patterns are not supported. These sets must not \
                       overlap between subtasks in the same plan. Defaults to an empty list when \
                       omitted, which is rejected as an unplanned subtask."
    )]
    pub touched_areas: Vec<String>,
}
