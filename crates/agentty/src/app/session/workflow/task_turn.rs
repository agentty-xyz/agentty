//! Isolated task execution on the owning session's serialized worker.

use std::sync::{Arc, Mutex};

use ag_contracts::{
    AgentRequestKind, LiveTranscript, PersonalityPrompt, TurnContinuation, TurnRequest,
};
use ag_session::{AgentAnswerContent, BuiltinAgentDefinition, SubmitAgentTaskRequest};
use ag_worker::RunClient;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::post_turn::{PostTurnContext, TurnFinalizerContext};
use super::task::{SessionTaskService, SessionTranscriptMessageAppend};
use super::worker::{SessionWorkerContext, TurnMetadata};
use super::{StatusTransition, isolation, post_turn, turn};
use crate::app::service::SessionRunFactory;
use crate::app::session::SessionError;
use crate::domain::permission::PermissionMode;
use crate::domain::session::Status;
use crate::domain::session_message::{SessionMessage, SessionMessageKind, SessionTranscript};
use crate::domain::turn_prompt::TurnPrompt;
use crate::infra::db::{TaskAgentHistoryRow, TaskAgentTurnMetadata};

/// Executes a role-bound attempt without reading or writing ordinary chat
/// state.
pub(super) async fn run(
    context: &SessionWorkerContext,
    run_client: Arc<dyn RunClient>,
    request: &SubmitAgentTaskRequest,
    factory: &dyn SessionRunFactory,
    cancellation: CancellationToken,
) -> Result<(), SessionError> {
    *context
        .cancel_token
        .lock()
        .map_err(|_| SessionError::Workflow("cancel token lock poisoned".to_string()))? =
        cancellation.clone();
    let transition = StatusTransition::from_parts(
        context.app_event_tx.clone(),
        Arc::clone(&context.clock),
        context.db.clone(),
        context.session_id.clone(),
        Arc::clone(&context.session_update_versions),
        Arc::clone(&context.status),
    );
    transition
        .apply_or_invalid_transition(Status::InProgress)
        .await?;
    let result = execute(context, run_client, request, factory, cancellation).await;
    let finalizer = TurnFinalizerContext::from_worker(context);
    let status_result = match &result {
        Ok(()) | Err(SessionError::StoppedByUser(_)) => Ok(Status::Review),
        Err(error) => Err(SessionError::Workflow(error.to_string())),
    };
    post_turn::finalize_channel_turn(&finalizer, &status_result).await;

    result
}

/// Task-only replay is a fixed snapshot, never the mutable session transcript.
#[derive(Debug)]
struct TaskReplay(String);

impl LiveTranscript for TaskReplay {
    fn replay_text(&self) -> Option<String> {
        Some(self.0.clone())
    }
}

async fn execute(
    context: &SessionWorkerContext,
    run_client: Arc<dyn RunClient>,
    request: &SubmitAgentTaskRequest,
    factory: &dyn SessionRunFactory,
    cancellation: CancellationToken,
) -> Result<(), SessionError> {
    let (history, prepared) = tokio::select! {
        biased;
        () = cancellation.cancelled() => return Err(stopped()),
        result = prepare(context, request) => result?,
    };
    let client = factory
        .create_task(&history.context.context)
        .map_err(turn::session_error_from_agent_error)?;
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let consumer = tokio::spawn(turn::consume_turn_events(
        event_rx,
        context.app_event_tx.clone(),
        context.session_id.clone(),
        Arc::clone(&context.child_pid),
    ));
    let permission_mode = prepared.permission_mode;
    let result = client
        .submit(prepared, event_tx, cancellation.clone())
        .await;
    let cleanup = client.shutdown().await;
    let activity = consumer.await.unwrap_or_default();
    activity.clear_live(&context.app_event_tx, &context.session_id);
    let result = result.map_err(turn::session_error_from_agent_error)?;
    cleanup.map_err(turn::session_error_from_agent_error)?;
    check_cancellation(&cancellation)?;
    let assignment = history.context.context.assignment;
    let instruction_conversation_id = if ag_worker::uses_persistent_session(assignment.agent.kind())
    {
        ag_contracts::normalize_instruction_conversation_id(
            result.provider_conversation_id.as_deref(),
        )
    } else {
        None
    };
    let is_recorded = context
        .db
        .tasks()
        .persist_agent_turn(
            &request.attempt_id,
            &TaskAgentTurnMetadata {
                assistant_message: serde_json::json!(result.assistant_message).to_string(),
                harness_turn_report: result.harness_report,
                instruction_conversation_id,
                input_tokens: result.input_tokens,
                output_tokens: result.output_tokens,
                provider_conversation_id: result.provider_conversation_id,
            },
        )
        .await?;
    append_chain_answer(context, request, &result.assistant_message, is_recorded).await?;
    // Task questions are retained in task history; they never replace the
    // ordinary session's pending question set or trigger implementation
    // effects.
    if assignment.definition == BuiltinAgentDefinition::Implementer
        && permission_mode != PermissionMode::ReadOnly
        && result.assistant_message.questions.is_empty()
    {
        let run_client = ag_worker::scoped_client(
            run_client,
            ag_worker::RunScope {
                cancellation: Some(cancellation.clone()),
                ..ag_worker::RunScope::default()
            },
        );
        let mut post = PostTurnContext::from_worker(context, run_client, cancellation.clone());
        post.transcript = Arc::new(Mutex::new(effect_transcript(
            &history,
            request,
            &result.assistant_message,
        )));
        let upstream = context.load_published_upstream_ref().await;
        post_turn::apply_successful_turn_effects(
            &post,
            TurnMetadata {
                published_upstream_ref: upstream,
                review_comment_thread_ids: Vec::new(),
                session_agent: assignment.agent,
            },
            &[],
            Status::Review,
            post_turn::TurnEffectPolicy::Task,
        )
        .await?;
    }
    check_cancellation(&cancellation)?;

    Ok(())
}

/// Mirrors a newly recorded chain-owned role answer into the session chat.
///
/// Only the first save of an attempt (`is_recorded`) appends, so redelivered
/// or duplicate saves never append twice. Standalone role tasks keep their
/// output out of the ordinary transcript.
async fn append_chain_answer(
    context: &SessionWorkerContext,
    request: &SubmitAgentTaskRequest,
    response: &ag_protocol::AgentResponse,
    is_recorded: bool,
) -> Result<(), SessionError> {
    let text = response.to_display_text();
    if !is_recorded || text.trim().is_empty() || !is_chain_owned(context, request).await? {
        return Ok(());
    }
    let encoded_answer = AgentAnswerContent {
        agent: request.assignment.definition,
        text,
    }
    .encode();
    SessionTaskService::append_session_transcript_message(
        &context.transcript,
        &context.db,
        &context.app_event_tx,
        &context.session_update_versions,
        &context.session_id,
        SessionTranscriptMessageAppend {
            kind: SessionMessageKind::AgentAnswer,
            raw_content: &encoded_answer,
        },
    )
    .await;

    Ok(())
}

/// Returns whether the request's task is one of a session task chain's roles.
async fn is_chain_owned(
    context: &SessionWorkerContext,
    request: &SubmitAgentTaskRequest,
) -> Result<bool, SessionError> {
    let chains = context
        .db
        .tasks()
        .list_task_chains(&context.session_id)
        .await?;

    Ok(chains.iter().any(|chain| {
        chain.planner_task_id == request.task_id || chain.implementer_task_id == request.task_id
    }))
}

async fn prepare(
    context: &SessionWorkerContext,
    request: &SubmitAgentTaskRequest,
) -> Result<(TaskAgentHistoryRow, TurnRequest), SessionError> {
    drop(context.branch_operation_lock.lock().await);
    context
        .db
        .tasks()
        .verify_agent_plan(&context.session_id, request)
        .await?;
    let env = isolation::validate_session_worktree(
        context.fs_client.as_ref(),
        context.git_client.as_ref(),
        &context.folder,
        &context.session_id,
    )
    .await?;
    let history = context
        .db
        .tasks()
        .load_agent_history(&request.task_id)
        .await?
        .ok_or_else(|| SessionError::Workflow("Task context is missing".to_string()))?;
    let assignment = history.context.context.assignment;
    let permission = turn::load_session_permission_mode(&context.db, &context.session_id).await?;
    let personality = task_personality(context).await?;
    let replay = replay(&history);
    let persistent = ag_worker::uses_persistent_session(assignment.agent.kind());
    let continuation = TurnContinuation::provider(
        replay
            .as_ref()
            .map(|text| Arc::new(TaskReplay(text.clone())) as Arc<dyn LiveTranscript>),
        history.instruction_conversation_id.clone(),
        history.provider_conversation_id.clone(),
        if persistent { None } else { replay },
    );
    let input = serde_json::json!(request.input).to_string();
    let prepared = TurnRequest {
        continuation,
        execution_policy: ag_contracts::ExecutionPolicy::default(),
        folder: env.folder,
        main_checkout_root: env.main_checkout,
        model: assignment.agent.model().provider_model_str().to_string(),
        permission_mode: assignment.definition.permission_mode(permission),
        personality,
        prompt: TurnPrompt::from_agent_data(format!(
            "{}\n\nExplicit task input (JSON data):\n{input}",
            assignment.definition.instructions()
        )),
        reasoning_level: turn::load_session_reasoning_level(&context.db, &context.session_id).await,
        request_kind: if history.turns.is_empty() {
            AgentRequestKind::SessionStart
        } else {
            AgentRequestKind::SessionResume
        },
        response_style: turn::load_session_response_style(&context.db, &context.session_id).await,
        speed_mode: turn::load_session_speed_mode(&context.db, &context.session_id).await,
    };

    Ok((history, prepared))
}

fn replay(history: &TaskAgentHistoryRow) -> Option<String> {
    if history.turns.is_empty() {
        return None;
    }
    let turns = history
        .turns
        .iter()
        .map(|turn| {
            serde_json::json!({
                "input": turn.input,
                "response": turn.assistant_message,
            })
        })
        .collect::<Vec<_>>();

    Some(serde_json::json!({"task_history": turns}).to_string())
}

fn effect_transcript(
    history: &TaskAgentHistoryRow,
    request: &SubmitAgentTaskRequest,
    response: &ag_protocol::AgentResponse,
) -> SessionTranscript {
    let mut transcript = SessionTranscript::ephemeral(Vec::<SessionMessage>::new());
    for turn in &history.turns {
        transcript.append_message(
            SessionMessageKind::UserPrompt,
            &serde_json::json!(turn.input).to_string(),
        );
        transcript.append_message(SessionMessageKind::AssistantAnswer, &turn.assistant_message);
    }
    transcript.append_message(
        SessionMessageKind::UserPrompt,
        &serde_json::json!(request.input).to_string(),
    );
    transcript.append_message(SessionMessageKind::AssistantAnswer, &response.answer);

    transcript
}

async fn task_personality(
    context: &SessionWorkerContext,
) -> Result<PersonalityPrompt, SessionError> {
    let state = context
        .db
        .sessions()
        .load_session_personality_state(&context.session_id)
        .await?;
    let Some(selected) = state.and_then(|state| state.personality_id) else {
        return Ok(PersonalityPrompt::cleared(true));
    };
    let personality = context
        .personality_catalog_client
        .resolve(context.folder.clone(), selected)
        .await;

    Ok(personality.map_or_else(
        || PersonalityPrompt::cleared(true),
        |personality| PersonalityPrompt::active(personality.prompt, true),
    ))
}

fn stopped() -> SessionError {
    SessionError::StoppedByUser("[Stopped] Task interrupted by user.".to_string())
}

fn check_cancellation(cancellation: &CancellationToken) -> Result<(), SessionError> {
    if cancellation.is_cancelled() {
        return Err(stopped());
    }

    Ok(())
}

#[cfg(test)]
#[path = "task_turn_test.rs"]
mod tests;
