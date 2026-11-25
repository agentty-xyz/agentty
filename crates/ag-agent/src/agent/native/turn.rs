//! Prompt assembly and result conversion shared by native session turns and
//! one-shot utility runs.

use std::pin::pin;

use ag_contracts::{
    AgentDefinitionPromptUpdate, OneShotRequest, OneShotSubmission, ProviderCallBudget, TurnEvent,
};
use ag_harness::{RunError, RunOutput, Unsettled};
use ag_protocol::{
    AgentResponse, ProtocolRequestProfile, ProtocolSchemaInstructionMode, TurnPromptAttachment,
};
use ag_session::AgentKind;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::activity::ActivityBridge;
use super::api_key::with_stored_api_keys;
use super::config::{self, HarnessContext, ModelSelection, NativeHarnessConfig, RunOptions};
use super::failure;
use super::sandbox::UTILITY_SANDBOX;
use super::turn_error::NativeTurnError;
use super::usage::UsageMeter;
use crate::agent::{
    InstructionDeliveryMode, PromptPreparationRequest, execution_policy, parse_turn_response,
    prepare_prompt_text,
};

/// Prompt inputs for one native turn.
pub(crate) struct NativePrompt<'a> {
    /// Instruction delivery selected from the session state.
    pub(crate) delivery: InstructionDeliveryMode,
    /// Current agent body for full bootstraps.
    pub(crate) agent_profile_prompt: Option<&'a str>,
    /// Agent change for continued sessions.
    pub(crate) agent_profile_update: &'a AgentDefinitionPromptUpdate,
    /// User or utility prompt text.
    pub(crate) prompt: &'a str,
    /// Protocol family of the turn.
    pub(crate) protocol_profile: ProtocolRequestProfile,
    /// Prior transcript for sessions restarted without harness history.
    pub(crate) replay_transcript: Option<&'a str>,
    /// Session worktree.
    pub(crate) workspace_root: &'a std::path::Path,
}

impl NativePrompt<'_> {
    /// Renders protocol instructions and replay context around the prompt.
    ///
    /// # Errors
    /// Returns an error when a prompt template fails to render.
    pub(crate) fn render(&self) -> Result<String, String> {
        prepare_prompt_text(PromptPreparationRequest {
            instruction_delivery_mode: self.delivery,
            agent_profile_prompt: self.agent_profile_prompt,
            agent_profile_update: self.agent_profile_update,
            prompt: self.prompt,
            protocol_profile: self.protocol_profile,
            replay_transcript: self.replay_transcript,
            schema_instruction_mode: ProtocolSchemaInstructionMode::TransportSchema,
            workspace_root: self.workspace_root,
        })
        .map_err(failure("Failed to render the harness prompt"))
    }
}

/// Rejects image attachments before a turn starts.
///
/// Pasted images live outside the worktree, which `read` cannot reach, and
/// `read` returns text rather than image content, so the model would never
/// see them.
///
/// # Errors
/// Returns an actionable error when `attachments` is not empty.
pub(crate) fn reject_attachments(attachments: &[TurnPromptAttachment]) -> Result<(), String> {
    if attachments.is_empty() {
        return Ok(());
    }

    Err(
        "Harness sessions do not support image attachments yet. Remove the pasted images and send \
         the prompt again."
            .to_string(),
    )
}

/// Parses the validated output of a settled harness run into the shared
/// protocol response.
///
/// # Errors
/// Returns an error when the schema-valid output still fails strict protocol
/// parsing.
pub(crate) fn parse_output(
    output: &RunOutput,
    profile: ProtocolRequestProfile,
) -> Result<AgentResponse, String> {
    let output = output.parse::<Value>().unwrap_or_default();

    parse_turn_response(AgentKind::Harness, &output.to_string(), profile)
}

/// Runs one stateless utility prompt with the request's permissions and
/// provider-call budget.
///
/// # Errors
/// Returns [`NativeTurnError::Stopped`] when cancelled, or a diagnostic for
/// unsupported controls, configuration, provider, settlement, or protocol
/// failures.
pub(crate) async fn run_one_shot(
    config: &NativeHarnessConfig,
    request: OneShotRequest,
    cancellation: CancellationToken,
) -> Result<OneShotSubmission, NativeTurnError> {
    reject_attachments(&request.attachments)?;
    execution_policy::validate(AgentKind::Harness, &request.execution_policy)
        .map_err(|error| error.to_string())?;
    let environment = config.environment();
    let selection = ModelSelection::resolve(&request.model, &environment)?;
    let environment = with_stored_api_keys(environment).await;
    let profile = request.request_kind.protocol_profile();
    let repository = config::repository(&request.folder, &environment)?;
    // Kept until the run settles; dropping it removes the run's temporary
    // directory.
    let (sandbox, _temporary_directory) = config
        .utility_sandbox(&request.folder, &environment)
        .await?;
    let options = RunOptions::new(profile, request.permission_mode, &environment, &sandbox)?;
    let running_context_window = config::running_context_window(&selection, &environment).await?;
    let activity_tx = request.activity_tx.clone();
    let harness = selection.harness(
        &HarnessContext {
            compacts: false,
            environment: &environment,
            reasoning_level: request.reasoning_level,
            repository: &repository,
            running_context_window,
            sandbox: UTILITY_SANDBOX,
            summary_model: None,
        },
        ActivityBridge::new(move |event| {
            if let (TurnEvent::Activity(activity), Some(activity_tx)) = (event, &activity_tx) {
                let _ = activity_tx.send(activity);
            }
        }),
    )?;
    let prompt = NativePrompt {
        delivery: InstructionDeliveryMode::BootstrapFull,
        agent_profile_prompt: None,
        agent_profile_update: &AgentDefinitionPromptUpdate::Unchanged,
        prompt: &request.prompt,
        protocol_profile: profile,
        replay_transcript: None,
        workspace_root: &request.folder,
    }
    .render()?;
    let meter = UsageMeter::new(&selection, &environment, running_context_window);
    charge_run(request.provider_call_budget.as_ref())?;
    let (stream, handle) = harness.run(options.request(prompt)).into_parts();
    let cancel = handle.cancel_token();
    let mut outcome = pin!(handle.outcome());
    // A cancelled run still resolves only once its effects settle.
    let (usage, outcome) = tokio::join!(meter.drain(stream), async {
        tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                cancel.cancel();
                outcome.await
            }
            outcome = &mut outcome => outcome,
        }
    });
    outcome
        .settlement
        .as_ref()
        .map_err(Unsettled::error)
        .map_err(cleanup_failure)?;
    let output = outcome.result.map_err(NativeTurnError::from_run)?;
    let response = parse_output(&output, profile)?;

    Ok(OneShotSubmission { response, usage })
}

/// Charges `budget` one provider call for a harness run about to start.
///
/// A native run counts as one call however many model requests it sends,
/// matching one CLI or app-server turn attempt, so a graph budget means the
/// same on every backend. Requests inside the run, including re-sent
/// requests and context summaries, are not charged again.
///
/// # Errors
/// Returns the budget's exhaustion diagnostic before the run starts.
pub(crate) fn charge_run(budget: Option<&ProviderCallBudget>) -> Result<(), String> {
    budget.map_or(Ok(()), |budget| {
        budget.consume().map_err(|error| error.to_string())
    })
}

/// Describes cleanup of a finished run that still failed to settle.
pub(crate) fn cleanup_failure(error: &RunError) -> String {
    format!("Harness turn cleanup failed: {error}")
}
