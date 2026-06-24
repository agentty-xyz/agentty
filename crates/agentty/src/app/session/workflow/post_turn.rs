//! Post-turn result application for session workers.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use ag_forge as forge;
use serde_json;
use tokio::sync::mpsc;

use super::task::SessionOutputMessageAppend;
use super::worker::{SessionWorkerContext, TurnMetadata};
use super::{SessionTaskService, published_branch};
use crate::app::AppEvent;
use crate::app::assist::AssistContext;
use crate::app::service::SessionUpdateVersionMap;
use crate::app::session::{Clock, SessionError, TurnAppliedState};
use crate::domain::session::{SessionFollowUpTask, SessionId, SessionStats, Status};
use crate::domain::session_message::SessionMessageKind;
use crate::domain::transcript_notice::TranscriptNotice;
use crate::infra::channel::{AgentError, TurnResult};
use crate::infra::db::{AppRepositories, SessionTurnMetadata};
use crate::infra::fs::FsClient;
use crate::infra::git::GitClient;
use crate::infra::{agent, channel};

/// Narrow dependency set used to apply a completed provider turn.
///
/// This context intentionally excludes channel execution, filesystem diff
/// refresh, and status mutation dependencies from the successful-turn path.
/// New post-turn effects should add dependencies here, or to a smaller nested
/// input, instead of widening [`SessionWorkerContext`].
pub(super) struct PostTurnContext {
    /// Reducer event sender used for output and post-turn projections.
    pub(super) app_event_tx: mpsc::UnboundedSender<AppEvent>,
    /// Shared child-process PID slot reused by auto-commit cancellation.
    pub(super) child_pid: Arc<Mutex<Option<u32>>>,
    /// Clock used by linked review-request metadata refresh.
    pub(super) clock: Arc<dyn Clock>,
    /// Repository bundle used for turn metadata, settings, and auto-commit.
    pub(super) db: AppRepositories,
    /// Session worktree folder used by auto-commit and auto-push effects.
    pub(super) folder: PathBuf,
    /// Git boundary used by auto-commit and published-branch auto-push.
    pub(super) git_client: Arc<dyn GitClient>,
    /// Shared transcript buffer receiving final response text and notices.
    pub(super) output: Arc<Mutex<String>>,
    /// In-memory queue checked before starting detached auto-push effects.
    pub(super) queued_messages: Arc<Mutex<VecDeque<channel::TurnPrompt>>>,
    /// Forge boundary used for optional linked PR/MR metadata refresh.
    pub(super) review_request_client: Arc<dyn forge::ReviewRequestClient>,
    /// Per-app session update versions shared with the main runtime.
    pub(super) session_update_versions: SessionUpdateVersionMap,
    /// Session identifier whose completed turn is being applied.
    pub(super) session_id: SessionId,
}

impl PostTurnContext {
    /// Clones the worker fields required by post-turn result application.
    pub(super) fn from_worker(context: &SessionWorkerContext) -> Self {
        Self {
            app_event_tx: context.app_event_tx.clone(),
            child_pid: Arc::clone(&context.child_pid),
            clock: Arc::clone(&context.clock),
            db: context.db.clone(),
            folder: context.folder.clone(),
            git_client: Arc::clone(&context.git_client),
            output: Arc::clone(&context.output),
            queued_messages: Arc::clone(&context.queued_messages),
            review_request_client: Arc::clone(&context.review_request_client),
            session_update_versions: context.session_update_versions.clone(),
            session_id: context.session_id.clone(),
        }
    }

    /// Returns whether follow-up prompts are waiting for inline drainage.
    ///
    /// Treats a poisoned queue lock as non-empty so detached post-turn effects
    /// do not start unless the worker can prove no queued user messages are
    /// waiting to run.
    fn has_queued_messages(&self) -> bool {
        self.queued_messages
            .lock()
            .map_or(true, |guard| !guard.is_empty())
    }
}

/// Narrow dependency set used after a turn result to refresh status and diff
/// projections.
pub(super) struct TurnFinalizerContext {
    /// Reducer event sender used for size and status updates.
    pub(super) app_event_tx: mpsc::UnboundedSender<AppEvent>,
    /// Clock used to timestamp status transitions.
    pub(super) clock: Arc<dyn Clock>,
    /// Repository bundle used to refresh persisted diff stats and status.
    pub(super) db: AppRepositories,
    /// Session worktree folder whose diff stats are refreshed.
    pub(super) folder: PathBuf,
    /// Filesystem boundary used by diff-stat refresh.
    pub(super) fs_client: Arc<dyn FsClient>,
    /// Git boundary used by diff-stat refresh.
    pub(super) git_client: Arc<dyn GitClient>,
    /// Per-app session update versions shared with the main runtime.
    pub(super) session_update_versions: SessionUpdateVersionMap,
    /// Session identifier whose final state is being refreshed.
    pub(super) session_id: SessionId,
    /// Shared status handle updated after the turn result is known.
    pub(super) status: Arc<Mutex<Status>>,
}

impl TurnFinalizerContext {
    /// Clones the worker fields required by turn finalization.
    pub(super) fn from_worker(context: &SessionWorkerContext) -> Self {
        Self {
            app_event_tx: context.app_event_tx.clone(),
            clock: Arc::clone(&context.clock),
            db: context.db.clone(),
            folder: context.folder.clone(),
            fs_client: Arc::clone(&context.fs_client),
            git_client: Arc::clone(&context.git_client),
            session_update_versions: context.session_update_versions.clone(),
            session_id: context.session_id.clone(),
            status: Arc::clone(&context.status),
        }
    }
}

/// Applies one successful turn result to persistence and returns the
/// corresponding reducer projection.
struct TurnPersistence<'a> {
    context: &'a PostTurnContext,
    session_model: crate::domain::agent::AgentModel,
}

impl TurnPersistence<'_> {
    /// Persists one completed turn and returns the reducer projection derived
    /// from the canonical stored values.
    async fn apply(
        &self,
        assistant_message: &agent::AgentResponse,
        input_tokens: u64,
        output_tokens: u64,
        provider_conversation_id: Option<&str>,
    ) -> Result<TurnAppliedState, SessionError> {
        let summary = persisted_session_summary_payload(assistant_message);
        let questions = assistant_message.question_items();
        let questions_json = if questions.is_empty() {
            String::new()
        } else {
            serde_json::to_string(&questions).unwrap_or_default()
        };
        let follow_up_tasks = turn_applied_follow_up_tasks(assistant_message);
        let persisted_follow_up_text = follow_up_tasks
            .iter()
            .map(|follow_up_task| follow_up_task.text.clone())
            .collect::<Vec<_>>();
        let token_usage_delta = SessionStats {
            added_lines: 0,
            deleted_lines: 0,
            input_tokens,
            output_tokens,
        };
        let instruction_conversation_id =
            if agent::transport_mode(self.session_model.kind()).uses_app_server() {
                agent::normalize_instruction_conversation_id(provider_conversation_id)
            } else {
                None
            };
        self.context
            .db
            .sessions()
            .persist_session_turn_metadata(
                &self.context.session_id,
                &SessionTurnMetadata {
                    instruction_conversation_id,
                    model: self.session_model.as_str().to_string(),
                    provider_conversation_id: provider_conversation_id.map(str::to_string),
                    questions_json,
                    summary: summary.clone(),
                    token_usage_delta: token_usage_delta.clone(),
                },
            )
            .await?;
        self.context
            .db
            .sessions()
            .replace_session_follow_up_tasks(&self.context.session_id, &persisted_follow_up_text)
            .await?;

        Ok(TurnAppliedState {
            follow_up_tasks,
            questions,
            summary: (!summary.is_empty()).then_some(summary),
            token_usage_delta,
        })
    }
}

/// Applies the turn result: appends the final response, persists follow-up
/// metadata, updates stats, and runs auto-commit. Returns `Ok(Status)` on
/// success or `Err(description)` on turn failure after appending the error to
/// session output.
///
/// The final parsed response appends non-empty protocol `answer` text once the
/// turn completes. When no `answer` text exists, worker output falls back to
/// joined question text so clarification prompts remain visible while
/// thought-only responses are not persisted as final transcript output.
///
/// The raw agent `summary` payload is stored only in the session row. The
/// reducer receives a matching [`TurnAppliedState`] projection so the active UI
/// can render the same summary and follow-up metadata without embedding a
/// second markdown copy into the transcript message store. If canonical
/// metadata persistence fails, the worker appends a recovery error, triggers
/// `RefreshSessions`, and skips reducer projection emission.
pub(super) async fn apply_turn_result(
    context: &PostTurnContext,
    turn_metadata: TurnMetadata,
    turn_result: Result<TurnResult, AgentError>,
) -> Result<Status, SessionError> {
    match turn_result {
        Ok(result) => apply_successful_turn_result(context, turn_metadata, result).await,
        Err(AgentError::InterruptedByUser(message)) => {
            append_turn_error(context, &message).await;

            Err(SessionError::StoppedByUser(message))
        }
        Err(error) => {
            let error_text = error.to_string();
            append_turn_error(context, &error_text).await;

            Err(SessionError::Workflow(error_text))
        }
    }
}

/// Refreshes durable session projections and status after a turn result.
pub(super) async fn finalize_channel_turn(
    context: &TurnFinalizerContext,
    result: &Result<Status, SessionError>,
) {
    if let Some((session_size, added_lines, deleted_lines)) =
        SessionTaskService::refresh_persisted_session_diff_stats(
            &context.db,
            context.fs_client.as_ref(),
            context.git_client.as_ref(),
            &context.session_id,
            &context.folder,
        )
        .await
    {
        // Fire-and-forget: receiver may be dropped during shutdown.
        let _ = context.app_event_tx.send(AppEvent::SessionSizeUpdated {
            added_lines,
            deleted_lines,
            session_id: context.session_id.clone(),
            session_size,
        });
    }

    if let Some(target_status) = status_update_after_turn_result(result) {
        // Best-effort: status transition failure is non-critical.
        let _ = SessionTaskService::update_status(
            &context.status,
            context.clock.as_ref(),
            &context.db,
            &context.app_event_tx,
            &context.session_update_versions,
            &context.session_id,
            target_status,
        )
        .await;
    }
}

/// Returns the status transition the worker should emit after a turn result.
///
/// User-stopped turns are finalized by the UI cancellation path, which has
/// already requested `Review` and signaled the worker. The worker therefore
/// skips its normal error fallback so the stopped turn cannot race with the
/// explicit UI status transition.
pub(super) fn status_update_after_turn_result(
    result: &Result<Status, SessionError>,
) -> Option<Status> {
    match result {
        Ok(status) => Some(*status),
        Err(SessionError::StoppedByUser(_)) => None,
        Err(_) => Some(Status::Review),
    }
}

/// Appends one terminal turn error to the live and persisted transcript.
async fn append_turn_error(context: &PostTurnContext, error_text: &str) {
    let message = format!("\n{}\n", error_text.trim());
    SessionTaskService::append_session_output(
        &context.output,
        &context.db,
        &context.app_event_tx,
        &context.session_update_versions,
        &context.session_id,
        &message,
    )
    .await;
}

/// Persists the successful turn payload, emits the reducer projection, and
/// runs the auto-commit workflow with the project's fast-model default before
/// returning the next session status.
async fn apply_successful_turn_result(
    context: &PostTurnContext,
    turn_metadata: TurnMetadata,
    result: TurnResult,
) -> Result<Status, SessionError> {
    let TurnResult {
        assistant_message,
        context_reset: _,
        input_tokens,
        output_tokens,
        provider_conversation_id,
    } = result;

    if let Some(message) = build_assistant_transcript_output(&assistant_message) {
        SessionTaskService::append_session_output_message(
            &context.output,
            &context.db,
            &context.app_event_tx,
            &context.session_update_versions,
            &context.session_id,
            SessionOutputMessageAppend {
                formatted_message: message.as_str(),
                kind: SessionMessageKind::AssistantAnswer,
                raw_content: message.as_str(),
            },
        )
        .await;
    }
    let turn_applied_state = match (TurnPersistence {
        context,
        session_model: turn_metadata.session_model,
    }
    .apply(
        &assistant_message,
        input_tokens,
        output_tokens,
        provider_conversation_id.as_deref(),
    )
    .await)
    {
        Ok(turn_applied_state) => turn_applied_state,
        Err(error) => {
            handle_turn_persistence_failure(context, &error).await;

            return Err(error);
        }
    };
    let target_status = if turn_applied_state.questions.is_empty() {
        Status::Review
    } else {
        Status::Question
    };
    // Fire-and-forget: receiver may be dropped during shutdown.
    let _ = context.app_event_tx.send(AppEvent::AgentResponseReceived {
        session_id: context.session_id.clone(),
        turn_applied_state,
    });
    let auto_commit_model = SessionTaskService::load_auto_commit_model_setting(
        &context.db,
        &context.session_id,
        turn_metadata.session_model,
    )
    .await;

    let commit_outcome = SessionTaskService::handle_auto_commit(AssistContext {
        app_event_tx: context.app_event_tx.clone(),
        child_pid: Arc::clone(&context.child_pid),
        db: context.db.clone(),
        folder: context.folder.clone(),
        git_client: Arc::clone(&context.git_client),
        id: context.session_id.to_string(),
        output: Arc::clone(&context.output),
        session_model: auto_commit_model,
        session_update_versions: context.session_update_versions.clone(),
    })
    .await;
    let review_request_commit_message = commit_outcome.map(|outcome| outcome.commit_message);
    start_published_branch_auto_push(context, turn_metadata, review_request_commit_message);

    Ok(target_status)
}

/// Starts the optional published-branch auto-push effect from explicit
/// post-turn inputs.
fn start_published_branch_auto_push(
    context: &PostTurnContext,
    turn_metadata: TurnMetadata,
    review_request_commit_message: Option<String>,
) {
    if context.has_queued_messages() {
        return;
    }

    let Some(published_upstream_ref) = turn_metadata.published_upstream_ref else {
        return;
    };

    published_branch::start_published_branch_auto_push(
        published_branch::PublishedBranchAutoPushStartInput {
            app_event_tx: context.app_event_tx.clone(),
            clock: Arc::clone(&context.clock),
            db: context.db.clone(),
            folder: context.folder.clone(),
            git_client: Arc::clone(&context.git_client),
            output: Arc::clone(&context.output),
            published_upstream_ref,
            review_request_client: Arc::clone(&context.review_request_client),
            review_request_commit_message,
            session_id: context.session_id.clone(),
            session_update_versions: context.session_update_versions.clone(),
        },
    );
}

/// Reconciles a failed turn-metadata write by surfacing the error and forcing
/// the next UI reload to prefer durable state.
async fn handle_turn_persistence_failure(context: &PostTurnContext, error: &SessionError) {
    let message = TranscriptNotice::TurnMetadataError.format(format!(
        "Failed to persist completed turn metadata: {error}"
    ));
    SessionTaskService::append_session_output(
        &context.output,
        &context.db,
        &context.app_event_tx,
        &context.session_update_versions,
        &context.session_id,
        &message,
    )
    .await;

    let _ = context.app_event_tx.send(AppEvent::RefreshSessions);
}

/// Builds the persisted transcript chunk for one parsed assistant response.
///
/// Prefers the top-level `answer` text so normal chat output stays concise.
/// Falls back to joined question text when no answer is present so
/// clarification prompts stay visible while thought-only responses are not
/// persisted as final transcript output.
pub(super) fn build_assistant_transcript_output(
    assistant_message: &agent::AgentResponse,
) -> Option<String> {
    let answer_text = assistant_message.to_answer_display_text();
    if !answer_text.trim().is_empty() {
        return Some(format!("{}\n\n", answer_text.trim_end()));
    }

    let question_text = assistant_message
        .question_items()
        .into_iter()
        .filter_map(|question_item| {
            let trimmed_question = question_item.text.trim();
            if trimmed_question.is_empty() {
                return None;
            }

            Some(trimmed_question.to_string())
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    if question_text.is_empty() {
        return None;
    }

    Some(format!("{question_text}\n\n"))
}

/// Serializes one assistant summary payload for session persistence.
///
/// Review-mode rendering uses the raw JSON object so it can display separate
/// `Current Turn` and `Session Changes` sections without reparsing answer
/// markdown.
pub(super) fn persisted_session_summary_payload(
    assistant_message: &agent::AgentResponse,
) -> String {
    assistant_message
        .summary
        .as_ref()
        .and_then(|summary| serde_json::to_string(summary).ok())
        .unwrap_or_default()
}

/// Builds the reducer-facing follow-up-task projection for one assistant
/// response.
fn turn_applied_follow_up_tasks(
    _assistant_message: &agent::AgentResponse,
) -> Vec<SessionFollowUpTask> {
    Vec::new()
}
