//! Explicit acceptance of an immutable Planner response version.

/// Producing task and attempt selected by the user for plan acceptance.
///
/// The host reads the plan body from the persisted response. Neither caller
/// text nor successful task completion establishes acceptance. Only the latest
/// successful, question-free Planner attempt is eligible.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcceptTaskPlanRequest {
    /// Exact response version to accept.
    pub attempt_id: String,
    /// Planner task that produced the version.
    pub task_id: String,
}
