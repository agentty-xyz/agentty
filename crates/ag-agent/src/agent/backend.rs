use std::error::Error;
use std::fmt;
use std::path::Path;
use std::process::Command;

use ag_protocol::TurnPromptAttachment;

use crate::channel::AgentRequestKind;
use crate::model::agent::ReasoningLevel;
use crate::model::permission::PermissionMode;
use crate::model::session::SpeedMode;

/// Maximum concurrent subagents requested from providers with a native limit.
///
/// Excludes the parent agent. Provider exceptions still apply; this is not a
/// host-wide CPU or memory limit.
pub(super) const MAX_CONCURRENT_SUBAGENTS: usize = 2;

/// Transport runtime used to execute turns for one backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentTransport {
    /// Provider runs through a managed session runtime.
    AppServer,
    /// Provider runs as direct CLI subprocess commands.
    Cli,
}

impl AgentTransport {
    /// Returns whether this transport uses managed runtime sessions.
    pub fn uses_app_server(self) -> bool {
        matches!(self, Self::AppServer)
    }
}

/// Prompt delivery mode used by one provider backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentPromptTransport {
    /// Prompt is passed inline through argv.
    Argv,
    /// Prompt is streamed through stdin.
    Stdin,
}

/// Managed-runtime thought-stream classification policy for one provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AppServerThoughtPolicy {
    /// Provider does not expose dedicated thought phases.
    None,
    /// Provider uses phase labels to distinguish thought chunks.
    PhaseLabel,
}

/// Request payload used to build provider transport commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildCommandRequest<'a> {
    /// Ordered local image attachments referenced from the prompt body.
    pub attachments: &'a [TurnPromptAttachment],
    /// Working directory where the command will run.
    pub folder: &'a Path,
    /// Main repository checkout that must remain read-only during the turn,
    /// when Agentty can resolve it.
    pub main_checkout_root: Option<&'a Path>,
    /// Provider-specific model identifier.
    pub model: &'a str,
    /// Filesystem and command permission policy for this turn.
    pub permission_mode: PermissionMode,
    /// Current personality body included during a full prompt bootstrap.
    pub personality_prompt: Option<&'a str>,
    /// User prompt to send.
    pub prompt: &'a str,
    /// Reasoning effort preference for this turn.
    ///
    /// Ignored by backends/models that do not support reasoning effort.
    pub reasoning_level: ReasoningLevel,
    /// Replayable transcript text captured when the turn was queued.
    pub replay_transcript: Option<&'a str>,
    /// Canonical request kind that drives execution and protocol semantics.
    pub request_kind: &'a AgentRequestKind,
    /// Response-speed preference for this turn.
    pub speed_mode: SpeedMode,
}

/// Error type for backend setup and command construction failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentBackendError {
    /// One-time backend setup failure.
    Setup(String),
    /// Per-command build failure.
    CommandBuild(String),
}

impl fmt::Display for AgentBackendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Setup(message) | Self::CommandBuild(message) => {
                write!(formatter, "{message}")
            }
        }
    }
}

impl Error for AgentBackendError {}

/// Builds and configures external agent CLI commands.
#[cfg_attr(any(test, feature = "test-utils"), mockall::automock)]
pub trait AgentBackend: Send + Sync {
    /// Performs one-time setup in an agent folder before first run.
    ///
    /// # Errors
    /// Returns an error when one-time backend setup cannot be completed.
    fn setup(&self, folder: &Path) -> Result<(), AgentBackendError>;

    /// Builds one provider transport command.
    ///
    /// CLI-backed providers return the per-turn subprocess command. Managed
    /// providers return the long-lived runtime command that owns later turn
    /// execution over its native protocol.
    ///
    /// # Errors
    /// Returns an error when prompt rendering or provider argument
    /// construction fails.
    fn build_command<'request>(
        &'request self,
        request: BuildCommandRequest<'request>,
    ) -> Result<Command, AgentBackendError>;
}

#[cfg(test)]
#[path = "backend_test.rs"]
mod tests;
