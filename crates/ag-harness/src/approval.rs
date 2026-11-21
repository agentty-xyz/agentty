//! Host approval of tool calls that change state, bounded by a decision
//! timeout.
//!
//! With an [`ApprovalHandler`] configured, every call whose [`ToolEffects`]
//! (as `ToolHandler::call_effects` reports them) are not
//! [`ToolEffects::ReadOnly`] waits for the handler's [`Decision`] before it
//! runs; read-only calls never ask. Without one, every tool the turn's
//! [`crate::ToolSet`] holds runs as before, and `bash` commands excluded
//! from the sandbox stay inside it.

use std::error::Error;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::engine::CatchUnwind;
use crate::event::{RunId, SessionId};
use crate::schema_contract::{OutputSchema, OutputValidationError};
use crate::tool::ToolDefinition;
use crate::tool_handler::ToolEffects;
use crate::turn::TurnError;

/// Time an [`ApprovalHandler`] has to decide when the host sets no other
/// with [`crate::HarnessBuilder::approval_timeout`].
pub(crate) const DEFAULT_APPROVAL_TIMEOUT: Duration = Duration::from_mins(5);

/// Decides whether a tool call that changes state runs.
///
/// Give one to [`crate::HarnessBuilder::approval`]. The harness asks it
/// about every call to a tool whose [`ToolEffects`] are not
/// [`ToolEffects::ReadOnly`], after emitting
/// [`crate::RunEventKind::ApprovalRequested`] when the run streams its
/// events, and waits for the decision at most the harness's approval
/// timeout: a handler that has not decided by then fails the run with
/// [`crate::ErrorKind::ApprovalTimeout`], and the call never runs.
/// Cancelling the run drops the pending decision. A handler that panics
/// fails the run as a tool failure. Read-only calls are never gated;
/// declare a tool [`ToolEffects::Writes`] to have its calls approved.
#[async_trait]
pub trait ApprovalHandler: Send + Sync {
    /// Returns whether the call `request` describes runs, runs with other
    /// arguments, or is answered with feedback instead.
    async fn decide(&self, request: &ApprovalRequest) -> Decision;
}

/// One tool call waiting for its [`Decision`].
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct ApprovalRequest {
    /// Arguments the model sent, as a JSON object.
    pub arguments: Value,
    /// Provider identifier of the call.
    pub call_id: String,
    /// For a call marked [`ToolEffects::ExecutesUnsandboxed`], the exact
    /// argument vector, executable first, it runs with outside the sandbox
    /// if [`Decision::AllowUnsandboxed`] approves it, with no shell between;
    /// `None` for every other call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,
    /// What the call may change, as its tool declares it for these
    /// arguments; allowing [`ToolEffects::ExecutesUnsandboxed`] lets the
    /// command run outside the sandbox with the host's full access.
    pub effects: ToolEffects,
    /// Run that made the call; `None` for a call outside a run, such as one
    /// from [`crate::Harness::turn`].
    pub run: Option<RunId>,
    /// Host-created session whose turn made the call; `None` for a
    /// stateless run or turn.
    pub session: Option<SessionId>,
    /// Function name of the called tool.
    pub tool: String,
}

/// An [`ApprovalHandler`]'s answer to one [`ApprovalRequest`].
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Decision {
    /// Runs the call with the model's arguments.
    Allow,
    /// Runs the call with these arguments instead of the model's. They must
    /// match the tool's parameters schema, or the run fails as a tool
    /// failure. The session keeps the model's original call, and the
    /// call's result tells the model the arguments that ran, as an
    /// `approved_arguments` field; context reduction still describes the
    /// call by the model's arguments.
    AllowWith(Value),
    /// Does not run the call; the model receives `feedback` as its result
    /// and the run continues.
    Deny {
        /// Why the call was denied, sent to the model.
        feedback: String,
    },
    /// Runs a call marked [`ToolEffects::ExecutesUnsandboxed`] outside the
    /// sandbox, with the harness process's full access and exactly the
    /// [`ApprovalRequest::command`] it shows; on any other call it acts as
    /// [`Self::Allow`]. Only this decision lets a command leave the sandbox:
    /// [`Self::Allow`] and [`Self::AllowWith`] run it inside, so a handler
    /// that allows `bash` by name never approves an escape.
    AllowUnsandboxed,
}

/// A harness's approval handler and the time it has to decide.
#[derive(Clone)]
pub(crate) struct Approval {
    handler: Arc<dyn ApprovalHandler>,
    timeout: Duration,
}

impl Approval {
    pub(crate) fn new(handler: Arc<dyn ApprovalHandler>, timeout: Duration) -> Self {
        Self { handler, timeout }
    }

    /// Returns whether a call to a tool with `effects` waits for a decision.
    pub(crate) fn requires(effects: ToolEffects) -> bool {
        effects != ToolEffects::ReadOnly
    }

    /// Asks the handler about `request`, a call to the tool `definition`
    /// describes, and returns how the call is answered.
    ///
    /// # Errors
    /// Returns [`TurnError::ApprovalTimeout`] when the handler does not
    /// decide in time, and [`TurnError::ToolFailed`] when it panics or
    /// replaces the arguments with ones the tool's parameters reject.
    pub(crate) async fn decide(
        &self,
        definition: &ToolDefinition,
        request: ApprovalRequest,
    ) -> Result<Approved, TurnError> {
        let decision =
            tokio::time::timeout(self.timeout, CatchUnwind(self.handler.decide(&request))).await;
        let tool_failed = |source: ApprovalFailure| TurnError::ToolFailed {
            source: Box::new(source),
            tool: request.tool.clone(),
        };
        let decision = match decision {
            Ok(Ok(decision)) => decision,
            Ok(Err(_)) => return Err(tool_failed(ApprovalFailure::Panicked)),
            Err(_) => {
                return Err(TurnError::ApprovalTimeout {
                    timeout: self.timeout,
                    tool: request.tool,
                });
            }
        };

        match decision {
            Decision::Allow => Ok(Approved::Run(request.arguments)),
            Decision::AllowUnsandboxed => Ok(Approved::RunUnsandboxed(request.arguments)),
            Decision::AllowWith(arguments) => {
                OutputSchema::new(definition.parameters().clone())
                    .map_err(|error| error.to_string())
                    .and_then(|schema| {
                        schema
                            .validate_value(&arguments)
                            .map_err(|error| match error {
                                OutputValidationError::SchemaViolation { path, reason } => {
                                    format!("{path}: {reason}")
                                }
                                _ => "they are too large".to_string(),
                            })
                    })
                    .map_err(|reason| tool_failed(ApprovalFailure::InvalidArguments(reason)))?;

                Ok(Approved::Replaced(arguments))
            }
            Decision::Deny { feedback } => Ok(Approved::Denied(feedback)),
        }
    }
}

/// How an approved or denied call is answered.
#[derive(Debug, PartialEq)]
pub(crate) enum Approved {
    /// The call runs with the model's arguments.
    Run(Value),
    /// The call runs with the model's arguments, outside the sandbox when
    /// it is marked [`ToolEffects::ExecutesUnsandboxed`].
    RunUnsandboxed(Value),
    /// The call runs with these replacement arguments, which its result
    /// reports to the model.
    Replaced(Value),
    /// The call does not run; the model receives this feedback.
    Denied(String),
}

/// Why an approval failed the turn as a tool failure.
#[derive(Debug)]
enum ApprovalFailure {
    /// The replacement arguments do not match the tool's parameters.
    InvalidArguments(String),
    /// The handler panicked.
    Panicked,
}

impl fmt::Display for ApprovalFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidArguments(reason) => write!(
                formatter,
                "approved arguments do not match the tool's parameters: {reason}"
            ),
            Self::Panicked => formatter.write_str("the approval handler panicked"),
        }
    }
}

impl Error for ApprovalFailure {}

#[cfg(test)]
#[path = "approval_test.rs"]
mod tests;
