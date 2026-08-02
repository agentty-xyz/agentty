//! Orchestration and orchestration-task lifecycle states.
//!
//! One orchestration groups the child sessions proposed by a single controller
//! plan. The orchestration row tracks whether that plan is still awaiting the
//! user's approval, actively fanning out, or settled; each task row tracks one
//! child session through creation, execution, and settlement.

use std::collections::HashSet;
use std::fmt;
use std::str::FromStr;

use super::session::Status as SessionStatus;

/// Lifecycle state for one controller-owned orchestration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrchestrationStatus {
    /// The plan is persisted and parked on a clarification question.
    AwaitingApproval,
    /// The plan is approved and its tasks are fanning out.
    Running,
    /// Cancellation is blocking new fan-out while active children stop.
    Canceling,
    /// Every task settled and one durable roll-up delivery is being claimed.
    Submitting,
    /// Every task settled and the controller received its roll-up turn.
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
                | OrchestrationStatus::Submitting
        )
    }
}

impl fmt::Display for OrchestrationStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            OrchestrationStatus::AwaitingApproval => "AwaitingApproval",
            OrchestrationStatus::Running => "Running",
            OrchestrationStatus::Canceling => "Canceling",
            OrchestrationStatus::Submitting => "Submitting",
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
            "Submitting" => Ok(OrchestrationStatus::Submitting),
            "Done" => Ok(OrchestrationStatus::Done),
            "Canceled" => Ok(OrchestrationStatus::Canceled),
            _ => Err(format!("Unknown orchestration status: {value}")),
        }
    }
}

/// Lifecycle state for one orchestration task and its child session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrchestrationTaskStatus {
    /// Persisted with the proposed plan, not yet approved or fanned out.
    Planned,
    /// The child session is being created and started.
    Creating,
    /// The child session is running its turn.
    Running,
    /// The child session parked on clarification questions.
    WaitingForInput,
    /// The child session finished and is ready for review or integration.
    Ready,
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
            SessionStatus::Review
            | SessionStatus::AgentReview
            | SessionStatus::Merged
            | SessionStatus::Done => Self::Ready,
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
                | OrchestrationTaskStatus::Failed
                | OrchestrationTaskStatus::Canceled
        )
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
                | OrchestrationTaskStatus::WaitingForInput
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
                OrchestrationTaskStatus::Planned
                    | OrchestrationTaskStatus::Ready
                    | OrchestrationTaskStatus::Failed
                    | OrchestrationTaskStatus::Canceled,
                OrchestrationTaskStatus::Creating
            ) | (
                OrchestrationTaskStatus::Creating | OrchestrationTaskStatus::WaitingForInput,
                OrchestrationTaskStatus::Running
            ) | (
                OrchestrationTaskStatus::Running,
                OrchestrationTaskStatus::WaitingForInput
            ) | (
                OrchestrationTaskStatus::Running | OrchestrationTaskStatus::WaitingForInput,
                OrchestrationTaskStatus::Ready
            ) | (
                OrchestrationTaskStatus::Planned
                    | OrchestrationTaskStatus::Creating
                    | OrchestrationTaskStatus::Running
                    | OrchestrationTaskStatus::WaitingForInput,
                OrchestrationTaskStatus::Failed | OrchestrationTaskStatus::Canceled
            )
        )
    }
}

/// Pure scheduling decision derived from one orchestration task snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OrchestrationScheduleDecision {
    /// Number of planned tasks that may claim a parallelism slot.
    pub spawn_count: usize,
    /// Whether every non-empty task has settled and roll-up can be claimed.
    pub should_submit: bool,
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
            spawn_count,
            should_submit,
        }
    }
}

/// Protocol-independent snapshot of one task proposed by an orchestration
/// controller.
pub struct OrchestrationPlanTask {
    /// Standalone task prompt delivered to the child session.
    pub prompt: String,
    /// Stable kebab-case identity used for retries.
    pub task_key: String,
    /// Short user-facing task title.
    pub title: String,
    /// Literal repository-relative files or directories owned by the task.
    pub touched_areas: Vec<String>,
}

/// Validates one proposed subtask set before application code persists it.
///
/// # Errors
///
/// Returns a user-facing reason when the plan is too small, incomplete, uses
/// invalid task keys or paths, or gives different tasks overlapping scopes.
pub fn validate_subtasks(subtasks: &[OrchestrationPlanTask], is_retry: bool) -> Result<(), String> {
    if subtasks.len() < 2 && !is_retry {
        return Err("a meaningful orchestration requires at least two subtasks.".to_string());
    }
    let mut task_keys = HashSet::new();
    let mut scopes = Vec::<(&str, String)>::new();
    for subtask in subtasks {
        if !is_kebab_case_task_key(&subtask.task_key)
            || !task_keys.insert(subtask.task_key.as_str())
        {
            return Err("every subtask needs a unique kebab-case task key.".to_string());
        }
        if subtask.prompt.trim().is_empty()
            || subtask.title.trim().is_empty()
            || subtask.touched_areas.is_empty()
        {
            return Err(format!(
                "subtask `{}` needs a title, standalone prompt, and touched areas.",
                subtask.task_key
            ));
        }
        for area in &subtask.touched_areas {
            let scope = normalized_scope(area).map_err(|reason| {
                format!(
                    "subtask `{}` has invalid touched area `{area}`: {reason}.",
                    subtask.task_key
                )
            })?;
            if let Some((other_key, _)) = scopes.iter().find(|(other_key, other_scope)| {
                *other_key != subtask.task_key && scopes_overlap(other_scope, &scope)
            }) {
                return Err(format!(
                    "subtasks `{other_key}` and `{}` overlap at `{area}`.",
                    subtask.task_key
                ));
            }
            scopes.push((subtask.task_key.as_str(), scope));
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

fn scopes_overlap(left: &str, right: &str) -> bool {
    left == right
        || left
            .strip_prefix(right)
            .is_some_and(|suffix| suffix.starts_with('/'))
        || right
            .strip_prefix(left)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

impl fmt::Display for OrchestrationTaskStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            OrchestrationTaskStatus::Planned => "Planned",
            OrchestrationTaskStatus::Creating => "Creating",
            OrchestrationTaskStatus::Running => "Running",
            OrchestrationTaskStatus::WaitingForInput => "WaitingForInput",
            OrchestrationTaskStatus::Ready => "Ready",
            OrchestrationTaskStatus::Failed => "Failed",
            OrchestrationTaskStatus::Canceled => "Canceled",
        };

        formatter.write_str(value)
    }
}

impl FromStr for OrchestrationTaskStatus {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "Planned" => Ok(OrchestrationTaskStatus::Planned),
            "Creating" => Ok(OrchestrationTaskStatus::Creating),
            "Running" => Ok(OrchestrationTaskStatus::Running),
            "WaitingForInput" => Ok(OrchestrationTaskStatus::WaitingForInput),
            "Ready" => Ok(OrchestrationTaskStatus::Ready),
            "Failed" => Ok(OrchestrationTaskStatus::Failed),
            "Canceled" => Ok(OrchestrationTaskStatus::Canceled),
            _ => Err(format!("Unknown orchestration task status: {value}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// Round-trips every orchestration status through its persisted form.
    fn test_orchestration_status_round_trips_persisted_values() {
        // Arrange
        let statuses = [
            OrchestrationStatus::AwaitingApproval,
            OrchestrationStatus::Running,
            OrchestrationStatus::Canceling,
            OrchestrationStatus::Submitting,
            OrchestrationStatus::Done,
            OrchestrationStatus::Canceled,
        ];

        // Act
        let round_tripped = statuses.map(|status| {
            status
                .to_string()
                .parse::<OrchestrationStatus>()
                .expect("status should parse")
        });

        // Assert
        assert_eq!(round_tripped, statuses);
        assert!("Unknown".parse::<OrchestrationStatus>().is_err());
    }

    #[test]
    /// Restricts restart re-linking to orchestrations that still need work.
    fn test_only_unsettled_orchestrations_are_active() {
        // Arrange / Act / Assert
        assert!(OrchestrationStatus::AwaitingApproval.is_active());
        assert!(OrchestrationStatus::Running.is_active());
        assert!(OrchestrationStatus::Canceling.is_active());
        assert!(OrchestrationStatus::Submitting.is_active());
        assert!(!OrchestrationStatus::Done.is_active());
        assert!(!OrchestrationStatus::Canceled.is_active());
    }

    #[test]
    /// Round-trips every task status through its persisted form.
    fn test_orchestration_task_status_round_trips_persisted_values() {
        // Arrange
        let statuses = [
            OrchestrationTaskStatus::Planned,
            OrchestrationTaskStatus::Creating,
            OrchestrationTaskStatus::Running,
            OrchestrationTaskStatus::WaitingForInput,
            OrchestrationTaskStatus::Ready,
            OrchestrationTaskStatus::Failed,
            OrchestrationTaskStatus::Canceled,
        ];

        // Act
        let round_tripped = statuses.map(|status| {
            status
                .to_string()
                .parse::<OrchestrationTaskStatus>()
                .expect("status should parse")
        });

        // Assert
        assert_eq!(round_tripped, statuses);
        assert!("Unknown".parse::<OrchestrationTaskStatus>().is_err());
    }

    #[test]
    /// Treats a canceled straggler as settled so fan-in is not blocked by
    /// out-of-band cancellation.
    fn test_settled_task_statuses_include_cancellation() {
        // Arrange / Act / Assert
        assert!(OrchestrationTaskStatus::Ready.is_settled());
        assert!(OrchestrationTaskStatus::Failed.is_settled());
        assert!(OrchestrationTaskStatus::Canceled.is_settled());
        assert!(!OrchestrationTaskStatus::Planned.is_settled());
        assert!(!OrchestrationTaskStatus::Creating.is_settled());
        assert!(!OrchestrationTaskStatus::Running.is_settled());
        assert!(!OrchestrationTaskStatus::WaitingForInput.is_settled());
    }

    #[test]
    /// Counts a task waiting for user input against the parallelism cap
    /// because it still owns a live child session and worktree.
    fn test_parallelism_slots_cover_every_live_child() {
        // Arrange / Act / Assert
        assert!(OrchestrationTaskStatus::Creating.occupies_parallelism_slot());
        assert!(OrchestrationTaskStatus::Running.occupies_parallelism_slot());
        assert!(OrchestrationTaskStatus::WaitingForInput.occupies_parallelism_slot());
        assert!(!OrchestrationTaskStatus::Planned.occupies_parallelism_slot());
        assert!(!OrchestrationTaskStatus::Ready.occupies_parallelism_slot());
        assert!(!OrchestrationTaskStatus::Failed.occupies_parallelism_slot());
        assert!(!OrchestrationTaskStatus::Canceled.occupies_parallelism_slot());
    }

    #[test]
    /// Allows the fan-out, question, settle, and retry transitions the
    /// coordinator drives, and rejects skipping creation.
    fn test_task_status_transitions_cover_fan_out_and_retry() {
        // Arrange / Act / Assert
        assert!(
            OrchestrationTaskStatus::Planned.can_transition_to(OrchestrationTaskStatus::Creating)
        );
        assert!(
            OrchestrationTaskStatus::Creating.can_transition_to(OrchestrationTaskStatus::Running)
        );
        assert!(
            OrchestrationTaskStatus::Running
                .can_transition_to(OrchestrationTaskStatus::WaitingForInput)
        );
        assert!(
            OrchestrationTaskStatus::WaitingForInput
                .can_transition_to(OrchestrationTaskStatus::Running)
        );
        assert!(OrchestrationTaskStatus::Running.can_transition_to(OrchestrationTaskStatus::Ready));
        assert!(
            OrchestrationTaskStatus::Running.can_transition_to(OrchestrationTaskStatus::Canceled)
        );
        assert!(
            OrchestrationTaskStatus::Failed.can_transition_to(OrchestrationTaskStatus::Creating)
        );
        assert!(OrchestrationTaskStatus::Ready.can_transition_to(OrchestrationTaskStatus::Ready));
        assert!(
            !OrchestrationTaskStatus::Planned.can_transition_to(OrchestrationTaskStatus::Running)
        );
        assert!(
            !OrchestrationTaskStatus::Canceled.can_transition_to(OrchestrationTaskStatus::Ready)
        );
    }

    #[test]
    /// Derives fan-out capacity and roll-up readiness from typed task states.
    fn test_orchestration_policy_schedules_available_slots_and_settlement() {
        // Arrange
        let active_statuses = [
            Some(OrchestrationTaskStatus::Running),
            Some(OrchestrationTaskStatus::WaitingForInput),
            Some(OrchestrationTaskStatus::Planned),
            Some(OrchestrationTaskStatus::Planned),
        ];
        let settled_statuses = [
            Some(OrchestrationTaskStatus::Ready),
            Some(OrchestrationTaskStatus::Failed),
            Some(OrchestrationTaskStatus::Canceled),
        ];
        let invalid_statuses = [Some(OrchestrationTaskStatus::Ready), None];

        // Act
        let active_decision = OrchestrationPolicy::schedule(3, &active_statuses);
        let settled_decision = OrchestrationPolicy::schedule(3, &settled_statuses);
        let empty_decision = OrchestrationPolicy::schedule(3, &[]);
        let invalid_decision = OrchestrationPolicy::schedule(3, &invalid_statuses);

        // Assert
        assert_eq!(
            active_decision,
            OrchestrationScheduleDecision {
                spawn_count: 1,
                should_submit: false,
            }
        );
        assert_eq!(
            settled_decision,
            OrchestrationScheduleDecision {
                spawn_count: 0,
                should_submit: true,
            }
        );
        assert_eq!(
            empty_decision,
            OrchestrationScheduleDecision {
                spawn_count: 0,
                should_submit: false,
            }
        );
        assert!(!invalid_decision.should_submit);
    }

    #[test]
    /// Maps every child-session lifecycle family into orchestration policy.
    fn test_task_status_from_child_status_covers_session_lifecycle() {
        // Arrange
        let cases = [
            (SessionStatus::Draft, OrchestrationTaskStatus::Running),
            (SessionStatus::InProgress, OrchestrationTaskStatus::Running),
            (SessionStatus::Queued, OrchestrationTaskStatus::Running),
            (SessionStatus::Rebasing, OrchestrationTaskStatus::Running),
            (SessionStatus::Merging, OrchestrationTaskStatus::Running),
            (
                SessionStatus::Question,
                OrchestrationTaskStatus::WaitingForInput,
            ),
            (SessionStatus::Review, OrchestrationTaskStatus::Ready),
            (SessionStatus::AgentReview, OrchestrationTaskStatus::Ready),
            (SessionStatus::Merged, OrchestrationTaskStatus::Ready),
            (SessionStatus::Done, OrchestrationTaskStatus::Ready),
            (SessionStatus::Canceled, OrchestrationTaskStatus::Failed),
        ];

        // Act / Assert
        for (session_status, expected_task_status) in cases {
            assert_eq!(
                OrchestrationTaskStatus::from_child_status(session_status),
                expected_task_status
            );
        }
    }
}
