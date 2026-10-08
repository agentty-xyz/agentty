//! Projects content-free harness lifecycle events onto Agentty turn events.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::SystemTime;

use ag_contracts::{ActivityEvent, ActivityKind, ActivityStatus, TurnEvent};
use ag_harness::lifecycle::{LifecycleEvent, LifecycleEventKind, LifecycleObserver, ToolErrorType};

/// Lifecycle observer that reports tool activity and model waits for one
/// harness attempt.
pub(crate) struct ActivityBridge {
    attempt_id: String,
    sink: Box<dyn Fn(TurnEvent) + Send + Sync>,
    tool_names: Mutex<HashMap<u64, String>>,
}

impl ActivityBridge {
    /// Creates a bridge that forwards projected events to `sink`.
    pub(crate) fn new(sink: impl Fn(TurnEvent) + Send + Sync + 'static) -> Self {
        Self {
            attempt_id: uuid::Uuid::new_v4().to_string(),
            sink: Box::new(sink),
            tool_names: Mutex::new(HashMap::new()),
        }
    }

    /// Emits one tool snapshot, remembering the requested name for later
    /// updates of the same call.
    fn tool(&self, tool_call_id: u64, requested_name: Option<&str>, status: ActivityStatus) {
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
            exit_code: None,
            id: tool_call_id.to_string(),
            kind: activity_kind(&name),
            name,
            observed_at: SystemTime::now(),
            parent_id: None,
            status,
        }));
    }
}

impl LifecycleObserver for ActivityBridge {
    fn observe(&self, event: LifecycleEvent) {
        match event.kind() {
            LifecycleEventKind::ModelRequestStarted { model, .. } => {
                let model = model.as_ref().map_or("the model", |model| model.model());
                (self.sink)(TurnEvent::ThoughtDelta(format!("Waiting for {model}")));
            }
            LifecycleEventKind::ToolRequested {
                tool_call_id,
                tool_name,
                ..
            } => self.tool(tool_call_id.get(), Some(tool_name), ActivityStatus::Running),
            LifecycleEventKind::ToolCompleted { tool_call_id, .. } => {
                self.tool(tool_call_id.get(), None, ActivityStatus::Completed);
            }
            LifecycleEventKind::ToolFailed {
                error_type: ToolErrorType::Cancelled,
                tool_call_id,
                ..
            } => self.tool(tool_call_id.get(), None, ActivityStatus::Interrupted),
            LifecycleEventKind::ToolDenied { tool_call_id, .. }
            | LifecycleEventKind::ToolFailed { tool_call_id, .. } => {
                self.tool(tool_call_id.get(), None, ActivityStatus::Failed);
            }
            _ => {}
        }
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
