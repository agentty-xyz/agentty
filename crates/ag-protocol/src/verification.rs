//! Orchestration verification decisions returned by controller turns.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Controller disposition for one settled orchestration task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(
    title = "VerificationVerdict",
    description = "Controller disposition for one settled orchestration task."
)]
pub enum VerificationVerdict {
    /// The task satisfies its approved acceptance criteria and may integrate.
    Pass,
    /// The task needs correction and must remain outside integration.
    Flag,
}

/// Structured controller verdict for one settled orchestration task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[schemars(
    title = "VerificationVerdictItem",
    description = "Structured verification decision for one task in an orchestration settlement \
                   turn."
)]
pub struct VerificationVerdictItem {
    /// Concise evidence or unmet requirement supporting the verdict.
    #[schemars(
        title = "reason",
        description = "Concise evidence for a pass or the concrete unmet requirement for a flag."
    )]
    pub reason: String,
    /// Stable task key copied exactly from the verification envelope.
    #[schemars(
        title = "task_key",
        description = "Stable task key copied exactly from the verification envelope."
    )]
    pub task_key: String,
    /// Whether the task may integrate or needs correction.
    #[schemars(
        title = "verdict",
        description = "Whether the task passes verification or needs correction."
    )]
    pub verdict: VerificationVerdict,
}
