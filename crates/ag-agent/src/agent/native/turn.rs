//! Prompt assembly, settlement, and result conversion shared by native
//! session turns and one-shot utility runs.

use ag_contracts::{
    OneShotRequest, OneShotSubmission, PersonalityPromptUpdate, SessionDiffState, SessionStats,
    TurnEvent,
};
use ag_harness::turn::TurnReport;
use ag_harness::{TurnControl, TurnOutcome};
use ag_protocol::{
    AgentResponse, ProtocolRequestProfile, ProtocolSchemaInstructionMode, TurnPromptAttachment,
};
use ag_session::AgentKind;
use tokio_util::sync::CancellationToken;

use super::activity::ActivityBridge;
use super::config::{self, HarnessContext, ModelSelection, NativeHarnessConfig};
use super::failure;
use crate::agent::{
    InstructionDeliveryMode, PromptPreparationRequest, execution_policy, parse_turn_response,
    prepare_prompt_text,
};

/// Prompt inputs for one native turn.
pub(crate) struct NativePrompt<'a> {
    /// Instruction delivery selected from the session state.
    pub(crate) delivery: InstructionDeliveryMode,
    /// Current personality body for full bootstraps.
    pub(crate) personality_prompt: Option<&'a str>,
    /// Personality change for continued sessions.
    pub(crate) personality_update: &'a PersonalityPromptUpdate,
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
            personality_prompt: self.personality_prompt,
            personality_update: self.personality_update,
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

/// Waits until a turn's persistence, writes, and commands settle, retrying
/// failed persistence and command cleanup once.
///
/// The harness blocks the session's admission until a failed cleanup is
/// retried successfully, so callers keep a failed `control` to settle again
/// before the session's next turn.
///
/// # Errors
/// Returns the settlement phase that still failed.
pub(crate) async fn settle(control: &TurnControl) -> Result<(), String> {
    let mut settlement = control.settled().await;
    if settlement.is_err() {
        settlement = control.retry_settlement().await;
        if settlement.is_ok() {
            settlement = control.settled().await;
        }
    }

    settlement.map_err(|error| format!("Harness turn cleanup failed: {error}"))
}

/// Parses a settled harness outcome into the shared protocol response.
///
/// # Errors
/// Returns an error when the schema-valid output still fails strict protocol
/// parsing.
pub(crate) fn parse_outcome(
    outcome: &TurnOutcome,
    profile: ProtocolRequestProfile,
) -> Result<AgentResponse, String> {
    parse_turn_response(AgentKind::Harness, &outcome.output().to_string(), profile)
}

/// Sums provider-reported input and output tokens across model requests.
pub(crate) fn token_usage(report: &TurnReport) -> (u64, u64) {
    report
        .model_requests()
        .iter()
        .filter_map(|request| request.completion()?.usage().copied())
        .fold((0, 0), |(input, output), usage| {
            (
                input + usage.input_tokens().unwrap_or(0),
                output + usage.output_tokens().unwrap_or(0),
            )
        })
}

/// Runs one stateless utility prompt with the request's permissions and
/// provider-call budget.
///
/// # Errors
/// Returns a diagnostic for unsupported controls, configuration, provider,
/// cancellation, settlement, or protocol failures.
pub(crate) async fn run_one_shot(
    config: &NativeHarnessConfig,
    request: OneShotRequest,
    cancellation: CancellationToken,
) -> Result<OneShotSubmission, String> {
    execution_policy::validate(AgentKind::Harness, &request.execution_policy)
        .map_err(|error| error.to_string())?;
    let environment = config.environment();
    let selection = ModelSelection::resolve(&request.model)?;
    let profile = request.request_kind.protocol_profile();
    let options = config::turn_options(profile, request.permission_mode, &environment)?;
    let repository = config::repository(&request.folder, &environment)?;
    let activity_tx = request.activity_tx.clone();
    let harness = selection.harness(
        &HarnessContext {
            environment: &environment,
            provider_call_budget: request.provider_call_budget.as_ref(),
            reasoning_level: request.reasoning_level,
            repository: &repository,
        },
        ActivityBridge::new(move |event| {
            if let (TurnEvent::Activity(activity), Some(activity_tx)) = (event, &activity_tx) {
                let _ = activity_tx.send(activity);
            }
        }),
    )?;
    let prompt = NativePrompt {
        delivery: InstructionDeliveryMode::BootstrapFull,
        personality_prompt: None,
        personality_update: &PersonalityPromptUpdate::Unchanged,
        prompt: &request.prompt,
        protocol_profile: profile,
        replay_transcript: None,
        workspace_root: &request.folder,
    }
    .render()?;
    let turn = harness.turn(prompt, options).start();
    let control = turn.control();
    let outcome = tokio::select! {
        biased;
        () = cancellation.cancelled() => Err("[Stopped] Agent run canceled".to_string()),
        outcome = turn => outcome.map_err(|error| format!("Harness turn failed: {error}")),
    };
    settle(&control).await?;
    let outcome = outcome?;
    let response = parse_outcome(&outcome, profile)?;
    let (input_tokens, output_tokens) = token_usage(outcome.report());

    Ok(OneShotSubmission {
        response,
        stats: SessionStats {
            added_lines: 0,
            deleted_lines: 0,
            diff_state: SessionDiffState::Unknown,
            input_tokens,
            output_tokens,
        },
    })
}
