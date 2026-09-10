//! Orchestration and orchestration-task lifecycle states.
//!
//! One orchestration groups the child sessions proposed by a single controller
//! plan. The orchestration row tracks whether that plan is still awaiting the
//! user's approval, actively fanning out, or settled; each task row tracks one
//! child session through creation, execution, and settlement.

use std::collections::HashSet;
use std::fmt;
use std::str::FromStr;

use crate::model::SessionStatus;

/// Maximum number of automatic focused-review remediation turns per managed
/// worker settlement wave.
pub const MAX_AUTOMATED_REVIEW_ITERATIONS: i64 = 3;

/// Execution behavior for one persisted orchestration task.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum OrchestrationTaskKind {
    /// Produces branch changes that proceed through review and integration.
    #[default]
    Implementation,
    /// Produces a read-only report whose temporary worktree is discarded.
    Research,
}

impl fmt::Display for OrchestrationTaskKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            OrchestrationTaskKind::Implementation => "Implementation",
            OrchestrationTaskKind::Research => "Research",
        };

        formatter.write_str(value)
    }
}

impl FromStr for OrchestrationTaskKind {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "Implementation" => Ok(OrchestrationTaskKind::Implementation),
            "Research" => Ok(OrchestrationTaskKind::Research),
            _ => Err(format!("Unknown orchestration task kind: {value}")),
        }
    }
}

/// Lifecycle state for one controller-owned orchestration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrchestrationStatus {
    /// The plan is persisted and parked on the campaign approval board.
    AwaitingApproval,
    /// The plan is approved and its tasks are fanning out.
    Running,
    /// Cancellation is blocking new fan-out while active children stop.
    Canceling,
    /// Every task settled and the controller is verifying the results.
    Verifying,
    /// Verification passed and user integration approval is required.
    AwaitingIntegration,
    /// Verified tasks are being merged or published in plan order.
    Integrating,
    /// Every verified task integrated and the campaign was archived.
    Done,
    /// The user canceled the orchestration or its controller session.
    Canceled,
}

impl OrchestrationStatus {
    /// Returns whether the orchestration is still open.
    ///
    /// An open plan blocks another plan from being persisted for the same
    /// controller, including while it waits for approval.
    pub fn is_active(self) -> bool {
        matches!(
            self,
            OrchestrationStatus::AwaitingApproval
                | OrchestrationStatus::Running
                | OrchestrationStatus::Canceling
                | OrchestrationStatus::Verifying
                | OrchestrationStatus::AwaitingIntegration
                | OrchestrationStatus::Integrating
        )
    }
}

impl fmt::Display for OrchestrationStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            OrchestrationStatus::AwaitingApproval => "AwaitingApproval",
            OrchestrationStatus::Running => "Running",
            OrchestrationStatus::Canceling => "Canceling",
            OrchestrationStatus::Verifying => "Verifying",
            OrchestrationStatus::AwaitingIntegration => "AwaitingIntegration",
            OrchestrationStatus::Integrating => "Integrating",
            OrchestrationStatus::Done => "Done",
            OrchestrationStatus::Canceled => "Canceled",
        };

        formatter.write_str(value)
    }
}

impl FromStr for OrchestrationStatus {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "AwaitingApproval" => Ok(OrchestrationStatus::AwaitingApproval),
            "Running" => Ok(OrchestrationStatus::Running),
            "Canceling" => Ok(OrchestrationStatus::Canceling),
            "Verifying" => Ok(OrchestrationStatus::Verifying),
            "AwaitingIntegration" => Ok(OrchestrationStatus::AwaitingIntegration),
            "Integrating" => Ok(OrchestrationStatus::Integrating),
            "Done" => Ok(OrchestrationStatus::Done),
            "Canceled" => Ok(OrchestrationStatus::Canceled),
            _ => Err(format!("Unknown orchestration status: {value}")),
        }
    }
}

/// User-selected destination for verified orchestration task branches.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IntegrationApproach {
    /// Merge each verified child branch into the campaign base locally.
    LocalMerge,
    /// Publish each verified child branch as a forge review request.
    ReviewRequest,
}

impl fmt::Display for IntegrationApproach {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            IntegrationApproach::LocalMerge => "LocalMerge",
            IntegrationApproach::ReviewRequest => "ReviewRequest",
        };

        formatter.write_str(value)
    }
}

impl FromStr for IntegrationApproach {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "LocalMerge" => Ok(IntegrationApproach::LocalMerge),
            "ReviewRequest" => Ok(IntegrationApproach::ReviewRequest),
            _ => Err(format!("Unknown integration approach: {value}")),
        }
    }
}

/// Lifecycle state for one orchestration task and its child session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrchestrationTaskStatus {
    /// Follow-up scope proposed by the controller and awaiting approval.
    Proposed,
    /// Persisted with the proposed plan, not yet approved or fanned out.
    Planned,
    /// The child session is being created and started.
    Creating,
    /// The child session is running its turn.
    Running,
    /// Focused review is running or awaiting a persisted result.
    Reviewing,
    /// The coordinator is applying one focused-review suggestion set.
    ReviewApplying,
    /// The child session parked on clarification questions.
    WaitingForInput,
    /// The child session finished and is ready for review or integration.
    Ready,
    /// A research child returned its report and its worktree was discarded.
    Reported,
    /// Approved feedback is being delivered to the existing managed child.
    ContinuationPending,
    /// Verification passed and this task awaits its integration gate.
    AwaitingIntegration,
    /// The coordinator has started branch merge or publication.
    Merging,
    /// Branch work was merged locally successfully.
    Integrated,
    /// The child branch was published and is waiting for its forge review
    /// request to merge.
    ReviewRequested,
    /// Integration failed and requires attention.
    IntegrationFailed,
    /// Ownership permanently transferred from the coordinator to the user.
    Detached,
    /// The child session failed, or a straggler was canceled out of band.
    Failed,
    /// The task was canceled as part of a cascade cancel.
    Canceled,
}

impl OrchestrationTaskStatus {
    /// Maps one observed child-session status into the task state owned by
    /// orchestration.
    pub fn from_child_status(status: SessionStatus) -> Self {
        match status {
            SessionStatus::Draft
            | SessionStatus::InProgress
            | SessionStatus::Queued
            | SessionStatus::Rebasing
            | SessionStatus::Merging => Self::Running,
            SessionStatus::Question => Self::WaitingForInput,
            SessionStatus::Review | SessionStatus::AgentReview => Self::Reviewing,
            SessionStatus::Merged | SessionStatus::Done => Self::Ready,
            SessionStatus::Canceled => Self::Failed,
        }
    }

    /// Returns whether the task reached a state that fan-in treats as settled.
    ///
    /// A canceled straggler counts as settled so out-of-band cancellation
    /// unblocks the roll-up instead of stalling it.
    pub fn is_settled(self) -> bool {
        matches!(
            self,
            OrchestrationTaskStatus::Ready
                | OrchestrationTaskStatus::Reported
                | OrchestrationTaskStatus::Integrated
                | OrchestrationTaskStatus::ReviewRequested
                | OrchestrationTaskStatus::IntegrationFailed
                | OrchestrationTaskStatus::Failed
                | OrchestrationTaskStatus::Canceled
                | OrchestrationTaskStatus::Detached
        )
    }

    /// Returns the concise user-facing label shown in campaign status output.
    pub fn campaign_label(self) -> &'static str {
        self.labels().1
    }

    /// Returns whether the task currently occupies a parallelism slot.
    ///
    /// A task waiting for user input still holds its child session and
    /// worktree, so it keeps consuming a slot until the user answers.
    pub fn occupies_parallelism_slot(self) -> bool {
        matches!(
            self,
            OrchestrationTaskStatus::Creating
                | OrchestrationTaskStatus::Running
                | OrchestrationTaskStatus::Reviewing
                | OrchestrationTaskStatus::ReviewApplying
                | OrchestrationTaskStatus::WaitingForInput
                | OrchestrationTaskStatus::ContinuationPending
                | OrchestrationTaskStatus::Merging
        )
    }

    /// Returns whether a transition to `next` is valid.
    ///
    /// Retry re-enters `Creating` from a settled state with the same task key,
    /// which is what makes replying "retry the failed tasks" a clean respawn
    /// rather than a duplicate fan-out.
    pub fn can_transition_to(self, next: OrchestrationTaskStatus) -> bool {
        if self == next {
            return true;
        }

        matches!(
            (self, next),
            (
                OrchestrationTaskStatus::Proposed,
                OrchestrationTaskStatus::Planned
            ) | (
                OrchestrationTaskStatus::Planned
                    | OrchestrationTaskStatus::Ready
                    | OrchestrationTaskStatus::Failed
                    | OrchestrationTaskStatus::Canceled
                    | OrchestrationTaskStatus::IntegrationFailed,
                OrchestrationTaskStatus::Creating
            ) | (
                OrchestrationTaskStatus::Creating | OrchestrationTaskStatus::WaitingForInput,
                OrchestrationTaskStatus::Running
            ) | (
                OrchestrationTaskStatus::Running,
                OrchestrationTaskStatus::Reviewing | OrchestrationTaskStatus::WaitingForInput
            ) | (
                OrchestrationTaskStatus::Running
                    | OrchestrationTaskStatus::Reviewing
                    | OrchestrationTaskStatus::WaitingForInput,
                OrchestrationTaskStatus::Ready | OrchestrationTaskStatus::Reported
            ) | (
                OrchestrationTaskStatus::Reviewing,
                OrchestrationTaskStatus::ReviewApplying
            ) | (
                OrchestrationTaskStatus::ReviewApplying,
                OrchestrationTaskStatus::Reviewing
                    | OrchestrationTaskStatus::WaitingForInput
                    | OrchestrationTaskStatus::Failed
            ) | (
                OrchestrationTaskStatus::Ready,
                OrchestrationTaskStatus::AwaitingIntegration
                    | OrchestrationTaskStatus::ContinuationPending
                    | OrchestrationTaskStatus::Detached
            ) | (
                OrchestrationTaskStatus::AwaitingIntegration,
                OrchestrationTaskStatus::Merging
                    | OrchestrationTaskStatus::ContinuationPending
                    | OrchestrationTaskStatus::Detached
            ) | (
                OrchestrationTaskStatus::IntegrationFailed,
                OrchestrationTaskStatus::ContinuationPending | OrchestrationTaskStatus::Detached
            ) | (
                OrchestrationTaskStatus::ContinuationPending,
                OrchestrationTaskStatus::Ready
                    | OrchestrationTaskStatus::WaitingForInput
                    | OrchestrationTaskStatus::Failed
            ) | (
                OrchestrationTaskStatus::Merging,
                OrchestrationTaskStatus::Integrated
                    | OrchestrationTaskStatus::ReviewRequested
                    | OrchestrationTaskStatus::IntegrationFailed
            ) | (
                OrchestrationTaskStatus::ReviewRequested,
                OrchestrationTaskStatus::Integrated | OrchestrationTaskStatus::IntegrationFailed
            ) | (
                OrchestrationTaskStatus::Planned
                    | OrchestrationTaskStatus::Creating
                    | OrchestrationTaskStatus::Running
                    | OrchestrationTaskStatus::Reviewing
                    | OrchestrationTaskStatus::ReviewApplying
                    | OrchestrationTaskStatus::WaitingForInput,
                OrchestrationTaskStatus::Failed | OrchestrationTaskStatus::Canceled
            )
        )
    }

    /// Returns whether this task no longer needs integration work.
    pub fn is_integration_settled(self) -> bool {
        matches!(
            self,
            OrchestrationTaskStatus::Integrated
                | OrchestrationTaskStatus::Detached
                | OrchestrationTaskStatus::Canceled
                | OrchestrationTaskStatus::Failed
        )
    }

    fn labels(self) -> (&'static str, &'static str) {
        match self {
            OrchestrationTaskStatus::Proposed => ("Proposed", "awaiting approval"),
            OrchestrationTaskStatus::Planned => ("Planned", "waiting"),
            OrchestrationTaskStatus::Creating => ("Creating", "starting"),
            OrchestrationTaskStatus::Running => ("Running", "running"),
            OrchestrationTaskStatus::Reviewing => ("Reviewing", "reviewing"),
            OrchestrationTaskStatus::ReviewApplying => ("ReviewApplying", "applying review"),
            OrchestrationTaskStatus::WaitingForInput => ("WaitingForInput", "waiting on you"),
            OrchestrationTaskStatus::Ready => ("Ready", "ready"),
            OrchestrationTaskStatus::Reported => ("Reported", "reported"),
            OrchestrationTaskStatus::ContinuationPending => ("ContinuationPending", "continuing"),
            OrchestrationTaskStatus::AwaitingIntegration => {
                ("AwaitingIntegration", "awaiting integration")
            }
            OrchestrationTaskStatus::Merging => ("Merging", "integrating"),
            OrchestrationTaskStatus::Integrated => ("Integrated", "integrated"),
            OrchestrationTaskStatus::ReviewRequested => ("ReviewRequested", "review requested"),
            OrchestrationTaskStatus::IntegrationFailed => {
                ("IntegrationFailed", "integration failed")
            }
            OrchestrationTaskStatus::Detached => ("Detached", "detached"),
            OrchestrationTaskStatus::Failed => ("Failed", "failed"),
            OrchestrationTaskStatus::Canceled => ("Canceled", "canceled"),
        }
    }
}

impl fmt::Display for OrchestrationTaskStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.labels().0)
    }
}

impl FromStr for OrchestrationTaskStatus {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "Proposed" => Ok(OrchestrationTaskStatus::Proposed),
            "Planned" => Ok(OrchestrationTaskStatus::Planned),
            "Creating" => Ok(OrchestrationTaskStatus::Creating),
            "Running" => Ok(OrchestrationTaskStatus::Running),
            "Reviewing" => Ok(OrchestrationTaskStatus::Reviewing),
            "ReviewApplying" => Ok(OrchestrationTaskStatus::ReviewApplying),
            "WaitingForInput" => Ok(OrchestrationTaskStatus::WaitingForInput),
            "Ready" => Ok(OrchestrationTaskStatus::Ready),
            "Reported" => Ok(OrchestrationTaskStatus::Reported),
            "ContinuationPending" => Ok(OrchestrationTaskStatus::ContinuationPending),
            "AwaitingIntegration" => Ok(OrchestrationTaskStatus::AwaitingIntegration),
            "Merging" => Ok(OrchestrationTaskStatus::Merging),
            "Integrated" => Ok(OrchestrationTaskStatus::Integrated),
            "ReviewRequested" => Ok(OrchestrationTaskStatus::ReviewRequested),
            "IntegrationFailed" => Ok(OrchestrationTaskStatus::IntegrationFailed),
            "Detached" => Ok(OrchestrationTaskStatus::Detached),
            "Failed" => Ok(OrchestrationTaskStatus::Failed),
            "Canceled" => Ok(OrchestrationTaskStatus::Canceled),
            _ => Err(format!("Unknown orchestration task status: {value}")),
        }
    }
}

/// Pure scheduling decision derived from one orchestration task snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OrchestrationScheduleDecision {
    /// Whether every non-empty task has settled and roll-up can be claimed.
    pub should_submit: bool,
    /// Number of planned tasks that may claim a parallelism slot.
    pub spawn_count: usize,
}

/// Pure orchestration policy over typed task observations.
pub struct OrchestrationPolicy;

impl OrchestrationPolicy {
    /// Decides fan-out capacity and roll-up readiness without persistence or
    /// runtime dependencies.
    pub fn schedule(
        max_parallelism: usize,
        task_statuses: &[Option<OrchestrationTaskStatus>],
    ) -> OrchestrationScheduleDecision {
        let occupied_slots = task_statuses
            .iter()
            .filter(|status| status.is_some_and(OrchestrationTaskStatus::occupies_parallelism_slot))
            .count();
        let planned_tasks = task_statuses
            .iter()
            .filter(|status| **status == Some(OrchestrationTaskStatus::Planned))
            .count();
        let spawn_count = max_parallelism
            .saturating_sub(occupied_slots)
            .min(planned_tasks);
        let should_submit = !task_statuses.is_empty()
            && task_statuses
                .iter()
                .all(|status| status.is_some_and(OrchestrationTaskStatus::is_settled));

        OrchestrationScheduleDecision {
            should_submit,
            spawn_count,
        }
    }
}

/// Protocol-independent snapshot of one task proposed by an orchestration
/// controller.
#[derive(Clone)]
pub struct OrchestrationPlanTask {
    /// Observable conditions checked during settlement verification.
    pub acceptance_criteria: Vec<String>,
    /// Whether the task implements changes or returns research findings.
    pub kind: OrchestrationTaskKind,
    /// Standalone task prompt delivered to the child session.
    pub prompt: String,
    /// Stable kebab-case identity used for retries.
    pub task_key: String,
    /// Short user-facing task title.
    pub title: String,
    /// Best-effort repository-relative files or directories expected to change.
    pub touched_areas: Vec<String>,
}

/// Validates one proposed subtask set before application code persists it.
///
/// # Errors
///
/// Returns a user-facing reason when the plan is too small, incomplete, or uses
/// invalid task keys or planning paths.
pub fn validate_subtasks(subtasks: &[OrchestrationPlanTask], is_retry: bool) -> Result<(), String> {
    let is_research_wave = !subtasks.is_empty()
        && subtasks
            .iter()
            .all(|subtask| subtask.kind == OrchestrationTaskKind::Research);
    let has_research = subtasks
        .iter()
        .any(|subtask| subtask.kind == OrchestrationTaskKind::Research);
    if has_research && !is_research_wave {
        return Err(
            "research and implementation tasks must be proposed in separate waves.".to_string(),
        );
    }

    if subtasks.len() < 2 && !is_retry && !is_research_wave {
        return Err("a meaningful orchestration requires at least two subtasks.".to_string());
    }

    let mut task_keys = HashSet::new();
    for subtask in subtasks {
        if !is_kebab_case_task_key(&subtask.task_key)
            || !task_keys.insert(subtask.task_key.as_str())
        {
            return Err("every subtask needs a unique kebab-case task key.".to_string());
        }

        if subtask.prompt.trim().is_empty()
            || subtask.title.trim().is_empty()
            || subtask
                .acceptance_criteria
                .iter()
                .all(|criterion| criterion.trim().is_empty())
        {
            return Err(format!(
                "subtask `{}` needs a title, standalone prompt, and acceptance criteria.",
                subtask.task_key
            ));
        }
        for area in subtask
            .touched_areas
            .iter()
            .filter(|_| subtask.kind == OrchestrationTaskKind::Implementation)
        {
            normalized_scope(area).map_err(|reason| {
                format!(
                    "subtask `{}` has invalid touched area `{area}`: {reason}.",
                    subtask.task_key
                )
            })?;
        }
    }

    Ok(())
}

fn is_kebab_case_task_key(task_key: &str) -> bool {
    !task_key.is_empty()
        && task_key.split('-').all(|segment| {
            !segment.is_empty()
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        })
}

fn normalized_scope(area: &str) -> Result<String, &'static str> {
    let normalized = area.trim().trim_start_matches("./").trim_end_matches('/');
    if normalized.is_empty()
        || normalized.starts_with('/')
        || normalized.split('/').any(|part| part == "..")
    {
        return Err("use a non-empty repository-relative path");
    }

    if normalized.contains(['*', '?', '[', ']', '{', '}']) {
        return Err("use a literal file or directory path; wildcard patterns are not supported");
    }

    Ok(normalized.to_string())
}

#[cfg(test)]
#[path = "orchestration_test.rs"]
mod tests;
