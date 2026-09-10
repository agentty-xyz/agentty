//! App-server RPC [`AgentChannel`] adapter.
//!
//! Delegates turn execution to [`AppServerClient`] and bridges
//! [`AppServerStreamEvent`]s to the unified [`TurnEvent`] stream.

use std::sync::Arc;

use ag_protocol::{AgentResponse, ProtocolRequestProfile, build_protocol_repair_prompt};
use tokio::sync::mpsc;

use crate::agent;
use crate::app_server::{AppServerClient, AppServerStreamEvent, AppServerTurnRequest};
use crate::channel::{
    AgentChannel, AgentError, AgentFuture, SessionRef, StartSessionRequest, TurnEvent, TurnRequest,
    TurnResult,
};
use crate::model::agent::AgentKind;

/// [`AgentChannel`] adapter backed by a persistent app-server session.
///
/// Turn execution is delegated to [`AppServerClient::run_turn`].
/// [`AppServerStreamEvent`]s emitted by the provider are bridged to
/// [`TurnEvent::ThoughtDelta`] values when transient loader text should be
/// updated.
pub(crate) struct AppServerAgentChannel {
    /// Provider-specific app-server client.
    client: Arc<dyn AppServerClient>,
    /// Provider kind routed through this channel instance.
    kind: AgentKind,
}

impl AppServerAgentChannel {
    /// Creates a new app-server channel backed by the given client.
    pub(crate) fn new(client: Arc<dyn AppServerClient>, kind: AgentKind) -> Self {
        Self { client, kind }
    }

    /// Bridges normal-turn PID and transient loader updates until the runtime
    /// drops its stream sender.
    fn bridge_turn_stream(
        kind: AgentKind,
        mut stream_rx: mpsc::UnboundedReceiver<AppServerStreamEvent>,
        events: mpsc::UnboundedSender<TurnEvent>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            while let Some(event) = stream_rx.recv().await {
                match event {
                    AppServerStreamEvent::PidUpdate(pid) => {
                        let _ = events.send(TurnEvent::PidUpdate(pid));
                    }
                    AppServerStreamEvent::AssistantMessage {
                        message,
                        phase,
                        is_delta,
                    } => {
                        let trimmed = message.trim_end();
                        if trimmed.trim().is_empty() {
                            continue;
                        }

                        if agent::is_app_server_thought_chunk(kind, is_delta, phase.as_deref()) {
                            // Fire-and-forget: receiver may be dropped during
                            // shutdown.
                            let _ = events.send(TurnEvent::ThoughtDelta(trimmed.to_string()));
                        }
                    }
                    AppServerStreamEvent::ProgressUpdate(progress) => {
                        let trimmed = progress.trim();
                        if trimmed.is_empty() {
                            continue;
                        }

                        // Fire-and-forget: receiver may be dropped during
                        // shutdown.
                        let _ = events.send(TurnEvent::ThoughtDelta(trimmed.to_string()));
                    }
                }
            }
        })
    }
}

impl AgentChannel for AppServerAgentChannel {
    /// Returns a [`SessionRef`] immediately; the app-server session is
    /// initialised lazily on the first turn.
    fn start_session(
        &self,
        req: StartSessionRequest,
    ) -> AgentFuture<Result<SessionRef, AgentError>> {
        let session_id = req.session_id;

        Box::pin(async move { Ok(SessionRef { session_id }) })
    }

    /// Runs one app-server turn and bridges stream events to [`TurnEvent`]s.
    ///
    /// Assistant stream chunks are never appended directly to the transcript.
    /// Instead, Codex thought-style deltas (`phase: thinking/plan`) and
    /// provider progress updates are bridged to [`TurnEvent::ThoughtDelta`] so
    /// the UI loader can reflect transient state while the final persisted
    /// output still comes only from the parsed [`TurnResult`].
    /// Every terminal error clears the tracked PID, including failed repair.
    ///
    /// # Errors
    /// Returns [`AgentError`] when [`AppServerClient::run_turn`] fails.
    fn run_turn(
        &self,
        session_id: String,
        req: TurnRequest,
        events: mpsc::UnboundedSender<TurnEvent>,
    ) -> AgentFuture<Result<TurnResult, AgentError>> {
        let client = Arc::clone(&self.client);
        let kind = self.kind;
        let error_events = events.clone();
        let turn = async move {
            let mut req = req;
            req.prompt = agent::apply_response_style_prompt(
                req.prompt,
                req.request_kind.protocol_profile(),
                req.response_style,
            )
            .map_err(|error| AgentError::Backend(error.to_string()))?;
            let continuation = req.continuation.into_parts();
            let request = AppServerTurnRequest {
                provider_call_budget: None,
                folder: req.folder,
                live_transcript: continuation.live_transcript,
                main_checkout_root: req.main_checkout_root,
                model: req.model,
                permission_mode: req.permission_mode,
                personality: req.personality,
                prompt: req.prompt,
                request_kind: req.request_kind,
                replay_transcript: continuation.replay_transcript,
                provider_conversation_id: continuation.provider_conversation_id,
                persisted_instruction_conversation_id: continuation
                    .persisted_instruction_conversation_id,
                reasoning_level: req.reasoning_level,
                session_id,
                speed_mode: req.speed_mode,
            };
            let protocol_profile = request.request_kind.protocol_profile();
            let repair_request = request.clone();
            let (stream_tx, stream_rx) = mpsc::unbounded_channel::<AppServerStreamEvent>();

            let bridge_handle = Self::bridge_turn_stream(kind, stream_rx, events.clone());

            let turn_result = client.run_turn(request, stream_tx).await;
            // Task join: panic in the spawned task is not recoverable here.
            let _ = bridge_handle.await;

            match turn_result {
                Ok(response) => {
                    // Fire-and-forget: receiver may be dropped during shutdown.
                    let _ = events.send(TurnEvent::PidUpdate(response.pid));
                    let parsed = parse_or_repair_app_server_response(
                        kind,
                        &response,
                        protocol_profile,
                        repair_request,
                        &client,
                        &events,
                    )
                    .await?;

                    Ok(TurnResult {
                        assistant_message: parsed.assistant_message,
                        context_reset: response.context_reset,
                        input_tokens: response.input_tokens + parsed.repair_input_tokens,
                        output_tokens: response.output_tokens + parsed.repair_output_tokens,
                        provider_conversation_id: parsed.provider_conversation_id,
                    })
                }
                Err(error) => Err(AgentError::AppServer(error)),
            }
        };

        Box::pin(async move {
            let result = turn.await;
            if result.is_err() {
                let _ = error_events.send(TurnEvent::PidUpdate(None));
            }

            result
        })
    }

    /// Shuts down the underlying app-server session.
    fn shutdown_session(&self, session_id: String) -> AgentFuture<Result<(), AgentError>> {
        let client = Arc::clone(&self.client);

        Box::pin(async move {
            client.shutdown_session(session_id).await;

            Ok(())
        })
    }
}

/// Aggregated result from parsing an app-server turn response, including
/// metadata from a repair turn when one was needed.
struct AppServerParsedTurnResult {
    /// Parsed agent response from the successful attempt.
    assistant_message: AgentResponse,
    /// Provider conversation id from the latest successful attempt,
    /// falling back to the original response when the repair turn does
    /// not produce one.
    provider_conversation_id: Option<String>,
    /// Additional input tokens consumed by a repair turn (zero when no
    /// repair was needed).
    repair_input_tokens: u64,
    /// Additional output tokens consumed by a repair turn (zero when no
    /// repair was needed).
    repair_output_tokens: u64,
}

/// Parses one app-server turn response strictly, falling back to a single
/// protocol-repair retry when the initial parse fails.
///
/// The repair prompt is sent as a follow-up turn on the same session so the
/// agent retains the original conversation context. When repair succeeds,
/// the returned metadata reflects the repair turn's provider conversation id
/// and token usage so the caller can propagate them correctly.
///
/// When repair is attempted, a concise [`TurnEvent::ThoughtDelta`] is emitted
/// so the user can see that schema repair is in progress. The parse error is
/// deliberately excluded: thought updates render as live loader lines, and the
/// error carries provider diagnostics that must not reach the UI.
/// Repair streams forward PID changes while withholding provider diagnostics;
/// the final repair response replaces the tracked PID before parsing its
/// output.
async fn parse_or_repair_app_server_response(
    kind: AgentKind,
    response: &crate::app_server::AppServerTurnResponse,
    protocol_profile: ProtocolRequestProfile,
    repair_request: AppServerTurnRequest,
    client: &Arc<dyn AppServerClient>,
    events: &mpsc::UnboundedSender<TurnEvent>,
) -> Result<AppServerParsedTurnResult, AgentError> {
    let parse_error =
        match agent::parse_turn_response(kind, &response.assistant_message, protocol_profile) {
            Ok(parsed) => {
                return Ok(AppServerParsedTurnResult {
                    assistant_message: parsed,
                    provider_conversation_id: response.provider_conversation_id.clone(),
                    repair_input_tokens: 0,
                    repair_output_tokens: 0,
                });
            }
            Err(error) => error,
        };

    let _ = events.send(TurnEvent::ThoughtDelta(format!(
        "Protocol parse error; retrying schema repair for {kind}."
    )));

    let repair_prompt = build_protocol_repair_prompt(&parse_error, &response.assistant_message)
        .map_err(AgentError::Backend)?;

    let repair_provider_conversation_id = response
        .provider_conversation_id
        .clone()
        .or_else(|| repair_request.provider_conversation_id.clone());

    let repair_turn_request = AppServerTurnRequest {
        provider_call_budget: repair_request.provider_call_budget,
        folder: repair_request.folder,
        live_transcript: None,
        main_checkout_root: repair_request.main_checkout_root,
        model: repair_request.model,
        permission_mode: repair_request.permission_mode,
        personality: crate::channel::PersonalityPrompt::default(),
        prompt: ag_protocol::TurnPrompt::from_agent_data(repair_prompt),
        request_kind: repair_request.request_kind,
        replay_transcript: None,
        provider_conversation_id: repair_provider_conversation_id,
        persisted_instruction_conversation_id: None,
        reasoning_level: repair_request.reasoning_level,
        session_id: repair_request.session_id,
        speed_mode: repair_request.speed_mode,
    };
    let (repair_stream_tx, mut repair_stream_rx) = mpsc::unbounded_channel();
    let repair_bridge = {
        let events = events.clone();

        tokio::spawn(async move {
            while let Some(event) = repair_stream_rx.recv().await {
                if let AppServerStreamEvent::PidUpdate(pid) = event {
                    let _ = events.send(TurnEvent::PidUpdate(pid));
                }
            }
        })
    };
    let repair_result = client.run_turn(repair_turn_request, repair_stream_tx).await;
    let _ = repair_bridge.await;
    let repair_result = repair_result.map_err(|error| {
        AgentError::Backend(format!(
            "{parse_error}\nprotocol repair transport failed: {error}"
        ))
    })?;
    let _ = events.send(TurnEvent::PidUpdate(repair_result.pid));

    let parsed =
        agent::parse_turn_response(kind, &repair_result.assistant_message, protocol_profile)
            .map_err(|error| {
                AgentError::Backend(format!(
                    "{parse_error}\nprotocol repair retry also failed: {error}"
                ))
            })?;

    Ok(AppServerParsedTurnResult {
        assistant_message: parsed,
        provider_conversation_id: repair_result
            .provider_conversation_id
            .or(response.provider_conversation_id.clone()),
        repair_input_tokens: repair_result.input_tokens,
        repair_output_tokens: repair_result.output_tokens,
    })
}

#[cfg(test)]
#[path = "app_server_test.rs"]
mod tests;
