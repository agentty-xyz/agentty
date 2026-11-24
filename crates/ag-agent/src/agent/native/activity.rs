//! Projects content-free harness lifecycle events onto Agentty turn events.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use ag_contracts::{ActivityEvent, ActivityFacts, ActivityFailure, ActivityStatus, TurnEvent};
use ag_harness::bash::CommandExit;
use ag_harness::lifecycle::{LifecycleEvent, LifecycleEventKind, LifecycleObserver, ToolErrorType};

use super::tool_description::ToolDescription;

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
    /// updates of the same call. Lifecycle events carry no arguments, so the
    /// row is classified by the tool's name.
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
        let id = tool_call_id.to_string();
        let description = ToolDescription::new(&id, &name, None);

        (self.sink)(TurnEvent::Activity(ActivityEvent {
            attempt_id: self.attempt_id.clone(),
            exit_code: update.exit_code,
            facts: Box::new(update.facts),
            id,
            input: None,
            kind: description.kind,
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
                exit,
                tool_call_id,
                ..
            } => self.tool(
                tool_call_id.get(),
                None,
                ToolUpdate::completed(*duration, exit.as_ref()),
            ),
            LifecycleEventKind::ToolFailed {
                error_type,
                tool_call_id,
                ..
            } => self.tool(tool_call_id.get(), None, ToolUpdate::failed(*error_type)),
            LifecycleEventKind::ToolDenied { tool_call_id, .. } => {
                let mut update = ToolUpdate::status(ActivityStatus::Failed);
                update.facts.failure = Some(ActivityFailure::Unavailable);
                self.tool(tool_call_id.get(), None, update);
            }
            _ => {}
        }
    }
}

/// What one tool lifecycle event reports about its call.
struct ToolUpdate {
    exit_code: Option<i64>,
    facts: ActivityFacts,
    status: ActivityStatus,
}

impl ToolUpdate {
    /// Reports only a lifecycle state.
    fn status(status: ActivityStatus) -> Self {
        Self {
            exit_code: None,
            facts: ActivityFacts::default(),
            status,
        }
    }

    /// Reports a call that completed after `duration`, with how a command
    /// exited.
    fn completed(duration: Duration, exit: Option<&CommandExit>) -> Self {
        let mut update = Self::status(ActivityStatus::Completed);
        update.facts.duration = Some(duration);
        if let Some(exit) = exit {
            update.exit_code = exit.exit_code.map(i64::from);
            update.facts.pipe_status = exit.pipe_status.iter().copied().map(i64::from).collect();
            update.facts.signal = exit.signal.map(i64::from);
        }

        update
    }

    /// Reports a call that failed with `error_type`: a cancelled call was
    /// interrupted, and any other failed for the reason the type names.
    fn failed(error_type: ToolErrorType) -> Self {
        if error_type == ToolErrorType::Cancelled {
            return Self::status(ActivityStatus::Interrupted);
        }
        let mut update = Self::status(ActivityStatus::Failed);
        update.facts.failure = Some(
            [
                (ToolErrorType::ApprovalDenied, ActivityFailure::Denied),
                (ToolErrorType::ApprovalTimeout, ActivityFailure::TimedOut),
            ]
            .into_iter()
            .find_map(|(known, failure)| (known == error_type).then_some(failure))
            .unwrap_or(ActivityFailure::Error),
        );

        update
    }
}

#[cfg(test)]
#[path = "activity_test.rs"]
mod tests;
