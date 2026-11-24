//! Projects content-free harness lifecycle events onto Agentty turn events.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use ag_contracts::{ActivityEvent, ActivityFacts, ActivityKind, ActivityStatus, TurnEvent};
use ag_harness::lifecycle::{LifecycleEvent, LifecycleEventKind, LifecycleObserver, ToolErrorType};

/// Lifecycle observer that reports model waits, and optionally tool
/// activity, for one harness attempt.
pub(crate) struct ActivityBridge {
    attempt_id: String,
    sink: Box<dyn Fn(TurnEvent) + Send + Sync>,
    tool_names: Mutex<HashMap<u64, String>>,
    /// Whether tool lifecycle events become activity rows.
    tool_rows: bool,
}

impl ActivityBridge {
    /// Creates a bridge that forwards model waits and tool rows to `sink`,
    /// for a run without an event stream, such as a utility turn.
    pub(crate) fn new(sink: impl Fn(TurnEvent) + Send + Sync + 'static) -> Self {
        Self::with_tool_rows(sink, true)
    }

    /// Creates a bridge that forwards only model waits to `sink`, for a run
    /// whose event stream reports its tool rows.
    pub(crate) fn model_waits(sink: impl Fn(TurnEvent) + Send + Sync + 'static) -> Self {
        Self::with_tool_rows(sink, false)
    }

    fn with_tool_rows(sink: impl Fn(TurnEvent) + Send + Sync + 'static, tool_rows: bool) -> Self {
        Self {
            attempt_id: uuid::Uuid::new_v4().to_string(),
            sink: Box::new(sink),
            tool_names: Mutex::new(HashMap::new()),
            tool_rows,
        }
    }

    /// Emits one tool snapshot, remembering the requested name for later
    /// updates of the same call.
    fn tool(&self, tool_call_id: u64, requested_name: Option<&str>, update: ToolUpdate) {
        let name = {
            let mut tool_names = self
                .tool_names
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(requested_name) = requested_name {
                tool_names.insert(tool_call_id, requested_name.to_string());
            }

            tool_names.get(&tool_call_id).cloned().unwrap_or_default()
        };

        (self.sink)(TurnEvent::Activity(ActivityEvent {
            attempt_id: self.attempt_id.clone(),
            exit_code: update.exit_code,
            facts: Box::new(ActivityFacts {
                duration: update.duration,
                pipe_status: update.pipe_status,
                ..ActivityFacts::default()
            }),
            id: tool_call_id.to_string(),
            input: None,
            kind: activity_kind(&name),
            name,
            observed_at: SystemTime::now(),
            output: None,
            parent_id: None,
            status: update.status,
        }));
    }
}

impl LifecycleObserver for ActivityBridge {
    fn observe(&self, event: LifecycleEvent) {
        if let LifecycleEventKind::ModelRequestStarted { model, .. } = event.kind() {
            let model = model.as_ref().map_or("the model", |model| model.model());
            (self.sink)(TurnEvent::ThoughtDelta(format!("Waiting for {model}")));

            return;
        }
        if !self.tool_rows {
            return;
        }
        match event.kind() {
            LifecycleEventKind::ToolRequested {
                tool_call_id,
                tool_name,
                ..
            } => self.tool(
                tool_call_id.get(),
                Some(tool_name),
                ToolUpdate::status(ActivityStatus::Running),
            ),
            LifecycleEventKind::ToolCompleted {
                duration,
                exit_code,
                pipe_status,
                tool_call_id,
                ..
            } => self.tool(
                tool_call_id.get(),
                None,
                ToolUpdate {
                    duration: Some(*duration),
                    exit_code: exit_code.map(i64::from),
                    pipe_status: pipe_status.iter().copied().map(i64::from).collect(),
                    status: ActivityStatus::Completed,
                },
            ),
            LifecycleEventKind::ToolFailed {
                error_type,
                tool_call_id,
                ..
            } => self.tool(
                tool_call_id.get(),
                None,
                ToolUpdate::status(failure_status(*error_type)),
            ),
            LifecycleEventKind::ToolDenied { tool_call_id, .. } => {
                self.tool(
                    tool_call_id.get(),
                    None,
                    ToolUpdate::status(ActivityStatus::Failed),
                );
            }
            _ => {}
        }
    }
}

/// What one tool lifecycle event reports about its call.
struct ToolUpdate {
    duration: Option<Duration>,
    exit_code: Option<i64>,
    pipe_status: Vec<i64>,
    status: ActivityStatus,
}

impl ToolUpdate {
    /// Reports only a lifecycle state.
    fn status(status: ActivityStatus) -> Self {
        Self {
            duration: None,
            exit_code: None,
            pipe_status: Vec::new(),
            status,
        }
    }
}

/// Returns the state of a call that failed with `error_type`: a cancelled
/// call was interrupted.
fn failure_status(error_type: ToolErrorType) -> ActivityStatus {
    match error_type {
        ToolErrorType::Cancelled => ActivityStatus::Interrupted,
        _ => ActivityStatus::Failed,
    }
}

/// Classifies one built-in harness tool.
fn activity_kind(tool_name: &str) -> ActivityKind {
    match tool_name {
        "bash" => ActivityKind::Command,
        "write" => ActivityKind::FileChange,
        _ => ActivityKind::Tool,
    }
}

#[cfg(test)]
#[path = "activity_test.rs"]
mod tests;
