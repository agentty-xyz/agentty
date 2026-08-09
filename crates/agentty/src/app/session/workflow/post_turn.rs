//! Post-turn result application for session workers.

use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use ag_agent as agent;
use ag_agent::{AgentError, OneShotClient, TurnResult};
use ag_forge as forge;
use ag_git::GitClient;
use ag_protocol::{AgentResponse, ReviewCommentOutcome};
use serde_json;
use tokio::sync::mpsc;
use tracing::warn;

use super::task::SessionTranscriptMessageAppend;
use super::worker::{SessionWorkerContext, TurnMetadata, has_unfinished_branch_operation};
use super::{SessionTaskService, StatusTransition, published_branch, turn};
use crate::app::assist::AssistContext;
use crate::app::service::SessionUpdateVersionMap;
use crate::app::session::{Clock, SessionError, TurnAppliedState};
use crate::app::{AppEvent, orchestration};
use crate::domain::session::{SessionFollowUpTask, SessionId, SessionRole, SessionStats, Status};
use crate::domain::session_message::{SessionMessageKind, SessionTranscript};
use crate::domain::transcript_notice::TranscriptNotice;
use crate::domain::turn_prompt::TurnPrompt;
use crate::infra::db::{AppRepositories, SessionTurnMetadata};
use crate::infra::fs::FsClient;

/// Personality state persisted after one successful main session turn.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct TurnPersonalityPersistence {
    /// Selected personality processed by the turn, including an unavailable
    /// selection whose prompt hash is `None`.
    pub(super) applied_personality_id: Option<String>,
    /// Fingerprint of the personality body delivered to the provider.
    pub(super) applied_personality_prompt_hash: Option<String>,
}

/// Narrow dependency set used to apply a completed provider turn.
///
/// This context intentionally excludes channel execution, filesystem diff
/// refresh, and status mutation dependencies from the successful-turn path.
/// New post-turn effects should add dependencies here, or to a smaller nested
/// input, instead of widening [`SessionWorkerContext`].
pub(super) struct PostTurnContext {
    /// Reducer event sender used for output and post-turn projections.
    pub(super) app_event_tx: mpsc::UnboundedSender<AppEvent>,
    /// Serializes post-turn publish ownership with queued branch operations.
    pub(super) branch_operation_lock: Arc<tokio::sync::Mutex<()>>,
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
    /// Provider-neutral boundary used by post-turn auto-commit prompts.
    pub(super) one_shot_client: Arc<dyn OneShotClient>,
    /// In-memory queue checked before starting detached auto-push effects.
    pub(super) queued_messages: Arc<Mutex<VecDeque<TurnPrompt>>>,
    /// Forge boundary used for optional linked PR/MR metadata refresh.
    pub(super) review_request_client: Arc<dyn forge::ReviewRequestClient>,
    /// Per-app session update versions shared with the main runtime.
    pub(super) session_update_versions: SessionUpdateVersionMap,
    /// Session identifier whose completed turn is being applied.
    pub(super) session_id: SessionId,
    /// Shared typed transcript snapshot mirrored to the render layer.
    pub(super) transcript: Arc<Mutex<SessionTranscript>>,
}

impl PostTurnContext {
    /// Clones the worker fields required by post-turn result application.
    pub(super) fn from_worker(
        context: &SessionWorkerContext,
        one_shot_client: Arc<dyn OneShotClient>,
    ) -> Self {
        Self {
            app_event_tx: context.app_event_tx.clone(),
            branch_operation_lock: Arc::clone(&context.branch_operation_lock),
            child_pid: Arc::clone(&context.child_pid),
            clock: Arc::clone(&context.clock),
            db: context.db.clone(),
            folder: context.folder.clone(),
            git_client: Arc::clone(&context.git_client),
            one_shot_client,
            queued_messages: Arc::clone(&context.queued_messages),
            review_request_client: Arc::clone(&context.review_request_client),
            session_update_versions: context.session_update_versions.clone(),
            session_id: context.session_id.clone(),
            transcript: Arc::clone(&context.transcript),
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

    /// Returns whether session sync is already queued or running on this
    /// worker, failing closed when persisted operation state cannot be read.
    async fn has_unfinished_branch_operation(&self) -> bool {
        let unfinished_operations = match self
            .db
            .operations()
            .load_unfinished_session_operations()
            .await
        {
            Ok(unfinished_operations) => unfinished_operations,
            Err(error) => {
                warn!(
                    session_id = %self.session_id,
                    %error,
                    "Skipping post-turn auto-push because unfinished session operations could not be loaded"
                );

                return true;
            }
        };

        has_unfinished_branch_operation(&unfinished_operations, self.session_id.as_str())
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
    personality: TurnPersonalityPersistence,
    session_agent: crate::domain::agent::AgentSelection,
}

impl TurnPersistence<'_> {
    /// Persists one completed turn and returns the reducer projection derived
    /// from the canonical stored values.
    async fn apply(
        &self,
        assistant_message: &AgentResponse,
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
        let token_usage_delta = SessionStats {
            added_lines: 0,
            deleted_lines: 0,
            diff_state: agent::SessionDiffState::Unknown,
            input_tokens,
            output_tokens,
        };
        let instruction_conversation_id =
            if agent::transport_mode(self.session_agent.kind()).uses_app_server() {
                agent::normalize_instruction_conversation_id(provider_conversation_id)
            } else {
                None
            };
        let session_model = self.session_agent.model();
        self.context
            .db
            .sessions()
            .persist_session_turn_metadata(
                &self.context.session_id,
                &SessionTurnMetadata {
                    applied_personality_id: self.personality.applied_personality_id.clone(),
                    applied_personality_prompt_hash: self
                        .personality
                        .applied_personality_prompt_hash
                        .clone(),
                    instruction_conversation_id,
                    model: session_model.as_str().to_string(),
                    provider_conversation_id: provider_conversation_id.map(str::to_string),
                    questions_json,
                    summary: summary.clone(),
                    token_usage_delta: token_usage_delta.clone(),
                },
            )
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
/// success or `Err(description)` on turn failure after appending the error as
/// a workflow notice.
///
/// The final parsed response appends non-empty protocol `answer` text once the
/// turn completes. When no `answer` text exists, worker output falls back to
/// joined question text so clarification prompts remain visible while
/// thought-only responses are not persisted as assistant messages.
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
    personality: TurnPersonalityPersistence,
    turn_result: Result<TurnResult, AgentError>,
) -> Result<Status, SessionError> {
    match turn_result {
        Ok(result) => {
            apply_successful_turn_result(context, turn_metadata, personality, result).await
        }
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
    let session_role = turn::load_session_role(&context.db, &context.session_id).await;
    if session_role.tracks_worktree_changes()
        && let Some(diff_stats) = SessionTaskService::refresh_persisted_session_diff_stats(
            &context.db,
            context.fs_client.as_ref(),
            context.git_client.as_ref(),
            &context.session_id,
            &context.folder,
        )
        .await
    {
        // Fire-and-forget: receiver may be dropped during shutdown.
        let _ = context
            .app_event_tx
            .send(AppEvent::SessionDiffStatsUpdated {
                diff_stats,
                session_id: context.session_id.clone(),
            });
    }
    if session_role.owns_branch_changes()
        && let Err(error) = orchestration::persist_managed_child_area_compliance(
            &context.db,
            context.git_client.as_ref(),
            &context.session_id,
            &context.folder,
        )
        .await
    {
        warn!(
            session_id = %context.session_id,
            %error,
            "Failed to refresh managed-child touched-area evidence"
        );
    }
    if session_role == SessionRole::OrchestrationResearcher {
        archive_research_diff(context).await;
    }

    if let Some(target_status) = status_update_after_turn_result(result) {
        // Best-effort: status transition failure is non-critical.
        let status_transition = StatusTransition::from_parts(
            context.app_event_tx.clone(),
            Arc::clone(&context.clock),
            context.db.clone(),
            context.session_id.clone(),
            Arc::clone(&context.session_update_versions),
            Arc::clone(&context.status),
        );
        let _ = status_transition.apply(target_status).await;
    }
}

/// Archives any observed researcher diff before its temporary worktree is
/// reclaimed.
///
/// Research sessions are provider-enforced read-only, but preserving an
/// unexpected diff makes a policy violation inspectable after cleanup.
async fn archive_research_diff(context: &TurnFinalizerContext) {
    let base_branch = match context
        .db
        .sessions()
        .get_session_base_branch(&context.session_id)
        .await
    {
        Ok(Some(base_branch)) => base_branch,
        Ok(None) => return,
        Err(error) => {
            warn!(
                session_id = %context.session_id,
                %error,
                "Failed to load research-session base branch before diff archival"
            );

            return;
        }
    };
    let archived_diff = match context
        .git_client
        .diff(context.folder.clone(), base_branch)
        .await
    {
        Ok(diff) => diff,
        Err(error) => {
            warn!(
                session_id = %context.session_id,
                %error,
                "Failed to archive research-session diff before worktree cleanup"
            );

            return;
        }
    };

    if let Err(error) = context
        .db
        .sessions()
        .update_session_archived_diff(&context.session_id, Some(archived_diff))
        .await
    {
        warn!(
            session_id = %context.session_id,
            %error,
            "Failed to persist archived research-session diff"
        );
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

/// Maximum characters of one turn error kept in the session transcript.
///
/// Transcript notices are rendered as chat content, so an unbounded error text
/// paints raw provider output into the session. Providers already bound their
/// own failure messages; this is the backstop that holds for every error path.
const TURN_ERROR_NOTICE_MAX_CHARS: usize = 800;

/// Appends one terminal turn error to the live and persisted transcript.
///
/// The notice is truncated to [`TURN_ERROR_NOTICE_MAX_CHARS`] so no failure can
/// dump a screenful of provider diagnostics into the chat.
async fn append_turn_error(context: &PostTurnContext, error_text: &str) {
    let message = format!("\n{}\n", truncate_turn_error_notice(error_text));
    SessionTaskService::append_workflow_notice(
        &context.transcript,
        &context.db,
        &context.app_event_tx,
        &context.session_update_versions,
        &context.session_id,
        &message,
    )
    .await;
}

/// Truncates one turn error to the transcript notice budget.
fn truncate_turn_error_notice(error_text: &str) -> String {
    let mut characters = error_text.trim().chars();
    let mut notice: String = characters
        .by_ref()
        .take(TURN_ERROR_NOTICE_MAX_CHARS)
        .collect();

    if characters.next().is_some() {
        notice.push_str("\n[error truncated]");
    }

    notice
}

/// Persists the successful turn payload, emits the reducer projection, and
/// runs the auto-commit workflow with the project's fast-model default before
/// returning the next session status.
async fn apply_successful_turn_result(
    context: &PostTurnContext,
    turn_metadata: TurnMetadata,
    personality: TurnPersonalityPersistence,
    result: TurnResult,
) -> Result<Status, SessionError> {
    let TurnResult {
        mut assistant_message,
        context_reset: _,
        input_tokens,
        output_tokens,
        provider_conversation_id,
    } = result;

    let db = &context.db;
    let session_id = &context.session_id;
    orchestration::persist_controller_plan(db, session_id, &mut assistant_message).await?;

    if let Some(message) = build_assistant_message_content(&assistant_message) {
        SessionTaskService::append_session_transcript_message(
            &context.transcript,
            &context.db,
            &context.app_event_tx,
            &context.session_update_versions,
            &context.session_id,
            SessionTranscriptMessageAppend {
                kind: SessionMessageKind::AssistantAnswer,
                raw_content: message.as_str(),
            },
        )
        .await;
    }
    let turn_applied_state = match (TurnPersistence {
        context,
        personality,
        session_agent: turn_metadata.session_agent,
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
    let review_comment_outcomes = valid_review_comment_outcomes(
        &turn_metadata.review_comment_thread_ids,
        &assistant_message.review_comment_outcomes,
    );
    // Fire-and-forget: receiver may be dropped during shutdown.
    let _ = context.app_event_tx.send(AppEvent::AgentResponseReceived {
        session_id: context.session_id.clone(),
        turn_applied_state,
    });
    let owns_branch_changes = session_owns_branch_changes(&context.db, &context.session_id).await;
    let commit_outcome = if owns_branch_changes {
        SessionTaskService::handle_auto_commit(AssistContext {
            app_event_tx: context.app_event_tx.clone(),
            child_pid: Arc::clone(&context.child_pid),
            db: context.db.clone(),
            folder: context.folder.clone(),
            git_client: Arc::clone(&context.git_client),
            id: context.session_id.to_string(),
            one_shot_client: Arc::clone(&context.one_shot_client),
            session_agent: turn_metadata.session_agent,
            session_update_versions: context.session_update_versions.clone(),
            transcript: Arc::clone(&context.transcript),
        })
        .await
    } else {
        None
    };
    let review_request_commit_message = commit_outcome.map(|outcome| outcome.commit_message);
    let review_request_session_summary = assistant_message
        .summary
        .as_ref()
        .map(|summary| summary.session.clone());
    if owns_branch_changes {
        start_published_branch_auto_push(
            context,
            turn_metadata,
            review_request_commit_message,
            review_request_session_summary,
            review_comment_outcomes,
        )
        .await;
    }
    if target_status.allows_review_actions() && has_review_ready_stacked_children(context).await {
        let _ = context
            .app_event_tx
            .send(AppEvent::StackedParentTurnCompleted {
                session_id: context.session_id.clone(),
            });
    }

    Ok(target_status)
}

async fn session_owns_branch_changes(db: &AppRepositories, session_id: &str) -> bool {
    turn::load_session_role(db, session_id)
        .await
        .owns_branch_changes()
}

/// Returns deduplicated outcomes whose thread identifiers were explicitly
/// allowlisted for this turn and whose replies are nonblank.
fn valid_review_comment_outcomes(
    allowed_thread_ids: &[String],
    outcomes: &[ReviewCommentOutcome],
) -> Vec<ReviewCommentOutcome> {
    let allowed_thread_ids = allowed_thread_ids
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let mut accepted_thread_ids = HashSet::new();

    outcomes
        .iter()
        .filter(|outcome| allowed_thread_ids.contains(outcome.thread_id.as_str()))
        .filter(|outcome| !outcome.reply.trim().is_empty())
        .filter(|outcome| accepted_thread_ids.insert(outcome.thread_id.clone()))
        .map(|outcome| ReviewCommentOutcome {
            reply: outcome.reply.trim().to_string(),
            resolution: outcome.resolution,
            thread_id: outcome.thread_id.clone(),
        })
        .collect()
}

/// Returns whether the completed session has materialized stacked children
/// whose persisted statuses parse to review-action-ready states.
async fn has_review_ready_stacked_children(context: &PostTurnContext) -> bool {
    let Ok(Some(project_id)) = context
        .db
        .sessions()
        .load_session_project_id(&context.session_id)
        .await
    else {
        return false;
    };
    let Ok(sessions) = context
        .db
        .sessions()
        .load_sessions_for_project(project_id)
        .await
    else {
        return false;
    };

    sessions.into_iter().any(|session| {
        session
            .parent_session_id
            .as_deref()
            .is_some_and(|parent_session_id| parent_session_id == context.session_id.as_str())
            && session
                .status
                .parse::<Status>()
                .is_ok_and(Status::allows_review_actions)
    })
}

/// Starts the optional published-branch auto-push effect from explicit
/// post-turn inputs.
async fn start_published_branch_auto_push(
    context: &PostTurnContext,
    turn_metadata: TurnMetadata,
    review_request_commit_message: Option<String>,
    review_request_session_summary: Option<String>,
    review_comment_outcomes: Vec<ReviewCommentOutcome>,
) {
    let Some(published_upstream_ref) = turn_metadata.published_upstream_ref else {
        return;
    };
    if context.has_queued_messages() {
        return;
    }
    let branch_operation_guard = Arc::clone(&context.branch_operation_lock)
        .lock_owned()
        .await;
    if context.has_unfinished_branch_operation().await {
        return;
    }

    published_branch::start_published_branch_auto_push(
        published_branch::PublishedBranchAutoPushStartInput {
            app_event_tx: context.app_event_tx.clone(),
            branch_operation_guard,
            clock: Arc::clone(&context.clock),
            db: context.db.clone(),
            folder: context.folder.clone(),
            git_client: Arc::clone(&context.git_client),
            one_shot_client: Arc::clone(&context.one_shot_client),
            published_upstream_ref,
            review_comment_outcomes,
            review_request_client: Arc::clone(&context.review_request_client),
            review_request_commit_message,
            session_agent: turn_metadata.session_agent,
            session_id: context.session_id.clone(),
            session_summary: review_request_session_summary,
            session_update_versions: context.session_update_versions.clone(),
            transcript: Arc::clone(&context.transcript),
        },
    );
}

/// Reconciles a failed turn-metadata write by surfacing the error and forcing
/// the next UI reload to prefer durable state.
async fn handle_turn_persistence_failure(context: &PostTurnContext, error: &SessionError) {
    let message = TranscriptNotice::TurnMetadataError.format(format!(
        "Failed to persist completed turn metadata: {error}"
    ));
    SessionTaskService::append_workflow_notice(
        &context.transcript,
        &context.db,
        &context.app_event_tx,
        &context.session_update_versions,
        &context.session_id,
        &message,
    )
    .await;

    let _ = context.app_event_tx.send(AppEvent::RefreshSessions);
}

/// Builds the persisted assistant message for one parsed response.
///
/// Prefers the top-level `answer` text so normal chat output stays concise.
/// Falls back to joined question text when no answer is present so
/// clarification prompts stay visible while thought-only responses are not
/// persisted as assistant messages.
pub(super) fn build_assistant_message_content(assistant_message: &AgentResponse) -> Option<String> {
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
pub(super) fn persisted_session_summary_payload(assistant_message: &AgentResponse) -> String {
    assistant_message
        .summary
        .as_ref()
        .and_then(|summary| serde_json::to_string(summary).ok())
        .unwrap_or_default()
}

/// Builds the reducer-facing follow-up-task projection for one assistant
/// response.
fn turn_applied_follow_up_tasks(_assistant_message: &AgentResponse) -> Vec<SessionFollowUpTask> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use ag_agent::MockOneShotClient;
    use ag_git::{GitError, MockGitClient};
    use ag_protocol::ReviewCommentResolution;
    use tracing::instrument::WithSubscriber;

    use super::*;
    use crate::domain::agent::{AgentKind, AgentModel, AgentSelection, ReasoningLevel, SpeedMode};
    use crate::infra::db::PersistedSessionCreation;

    async fn insert_research_session(database: &AppRepositories, session_id: &str) {
        let project_id = database
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to upsert project");
        database
            .sessions()
            .insert_session_with_agent(PersistedSessionCreation {
                agent: "codex",
                base_branch: "main",
                id: session_id,
                is_draft: false,
                model: AgentKind::Codex.default_model().as_str(),
                orchestration_task_id: None,
                parent_session_id: None,
                personality_id: None,
                project_id,
                reasoning_level: ReasoningLevel::default(),
                role: Some("OrchestrationResearcher"),
                speed_mode: SpeedMode::Normal,
                status: "InProgress",
            })
            .await
            .expect("failed to insert research session");
    }

    fn research_finalizer_context(
        db: AppRepositories,
        folder: PathBuf,
        fs_client: crate::infra::fs::MockFsClient,
        git_client: MockGitClient,
        session_id: &str,
    ) -> (TurnFinalizerContext, Arc<Mutex<Status>>) {
        let status = Arc::new(Mutex::new(Status::InProgress));
        let context = TurnFinalizerContext {
            app_event_tx: mpsc::unbounded_channel().0,
            clock: Arc::new(crate::infra::clock::RealClock),
            db,
            folder,
            fs_client: Arc::new(fs_client),
            git_client: Arc::new(git_client),
            session_update_versions: Arc::default(),
            session_id: session_id.into(),
            status: Arc::clone(&status),
        };

        (context, status)
    }

    #[test]
    fn test_truncate_turn_error_notice_keeps_short_errors_intact() {
        // Arrange
        let error_text = "  Agent command failed with exit code 1.  ";

        // Act
        let notice = truncate_turn_error_notice(error_text);

        // Assert
        assert_eq!(notice, "Agent command failed with exit code 1.");
    }

    #[test]
    fn test_truncate_turn_error_notice_bounds_long_provider_dumps() {
        // Arrange
        let error_text = "x".repeat(TURN_ERROR_NOTICE_MAX_CHARS + 50);

        // Act
        let notice = truncate_turn_error_notice(&error_text);

        // Assert
        assert_eq!(
            notice.chars().count(),
            TURN_ERROR_NOTICE_MAX_CHARS + "\n[error truncated]".chars().count()
        );
        assert!(notice.ends_with("\n[error truncated]"));
    }

    #[test]
    fn test_valid_review_comment_outcomes_filters_and_normalizes_agent_output() {
        // Arrange
        let allowed_thread_ids = vec!["thread-fixed".to_string(), "thread-other".to_string()];
        let outcomes = vec![
            ReviewCommentOutcome {
                reply: "  Applied the validation.  ".to_string(),
                resolution: ReviewCommentResolution::Fixed,
                thread_id: "thread-fixed".to_string(),
            },
            ReviewCommentOutcome {
                reply: "Duplicate outcome.".to_string(),
                resolution: ReviewCommentResolution::Fixed,
                thread_id: "thread-fixed".to_string(),
            },
            ReviewCommentOutcome {
                reply: "No change needed.".to_string(),
                resolution: ReviewCommentResolution::NoChangeNeeded,
                thread_id: "thread-other".to_string(),
            },
            ReviewCommentOutcome {
                reply: "Unknown thread.".to_string(),
                resolution: ReviewCommentResolution::Fixed,
                thread_id: "thread-unknown".to_string(),
            },
            ReviewCommentOutcome {
                reply: "   ".to_string(),
                resolution: ReviewCommentResolution::Fixed,
                thread_id: "thread-other".to_string(),
            },
        ];

        // Act
        let accepted = valid_review_comment_outcomes(&allowed_thread_ids, &outcomes);

        // Assert
        assert_eq!(
            accepted,
            vec![
                ReviewCommentOutcome {
                    reply: "Applied the validation.".to_string(),
                    resolution: ReviewCommentResolution::Fixed,
                    thread_id: "thread-fixed".to_string(),
                },
                ReviewCommentOutcome {
                    reply: "No change needed.".to_string(),
                    resolution: ReviewCommentResolution::NoChangeNeeded,
                    thread_id: "thread-other".to_string(),
                }
            ]
        );
    }

    #[tokio::test]
    async fn test_unfinished_rebase_check_fails_closed_when_operation_query_fails() {
        // Arrange
        let (db, pool) = AppRepositories::in_memory_with_pool().await;
        pool.close().await;
        let context = PostTurnContext {
            app_event_tx: mpsc::unbounded_channel().0,
            branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::infra::clock::RealClock),
            db,
            folder: PathBuf::new(),
            git_client: Arc::new(MockGitClient::new()),
            one_shot_client: Arc::new(MockOneShotClient::new()),
            queued_messages: Arc::new(Mutex::new(VecDeque::new())),
            review_request_client: Arc::new(forge::MockReviewRequestClient::new()),
            session_update_versions: Arc::default(),
            session_id: "session-id".into(),
            transcript: Arc::new(Mutex::new(SessionTranscript::default())),
        };

        // Act
        let should_skip_auto_push = context.has_unfinished_branch_operation().await;

        // Assert
        assert!(
            should_skip_auto_push,
            "operation-query failures must suppress post-turn auto-push"
        );
    }

    #[tokio::test]
    async fn finalization_tolerates_managed_child_evidence_persistence_failure() {
        // Arrange
        let (db, pool) = AppRepositories::in_memory_with_pool().await;
        pool.close().await;
        let status = Arc::new(Mutex::new(Status::InProgress));
        let context = TurnFinalizerContext {
            app_event_tx: mpsc::unbounded_channel().0,
            clock: Arc::new(crate::infra::clock::RealClock),
            db,
            folder: PathBuf::new(),
            fs_client: Arc::new(crate::infra::fs::MockFsClient::new()),
            git_client: Arc::new(MockGitClient::new()),
            session_update_versions: Arc::default(),
            session_id: "session-id".into(),
            status: Arc::clone(&status),
        };
        let result = Err(SessionError::StoppedByUser("stopped".to_string()));

        // Act
        finalize_channel_turn(&context, &result).await;

        // Assert
        assert_eq!(
            *status.lock().expect("status lock should remain usable"),
            Status::InProgress
        );
    }

    #[tokio::test]
    async fn finalization_archives_research_diff_before_status_transition() {
        // Arrange
        let db = AppRepositories::in_memory().await;
        insert_research_session(&db, "research-session").await;
        let mut fs_client = crate::infra::fs::MockFsClient::new();
        fs_client.expect_is_dir().times(1).return_const(false);
        let mut git_client = MockGitClient::new();
        git_client.expect_diff().times(1).returning(|folder, base| {
            assert_eq!(folder, PathBuf::from("/tmp/research-session"));
            assert_eq!(base, "main");

            Box::pin(async {
                Ok("diff --git a/violation.txt b/violation.txt\n+unexpected\n".to_string())
            })
        });
        let (context, status) = research_finalizer_context(
            db.clone(),
            PathBuf::from("/tmp/research-session"),
            fs_client,
            git_client,
            "research-session",
        );

        // Act
        finalize_channel_turn(&context, &Ok(Status::Review)).await;
        let archived_diff = db
            .sessions()
            .load_session_archived_diff("research-session")
            .await
            .expect("failed to load archived diff");

        // Assert
        assert_eq!(
            archived_diff.as_deref(),
            Some("diff --git a/violation.txt b/violation.txt\n+unexpected\n")
        );
        assert_eq!(
            *status.lock().expect("status lock should remain usable"),
            Status::Review
        );
    }

    #[tokio::test]
    async fn research_diff_archival_returns_when_base_branch_is_missing() {
        // Arrange
        let db = AppRepositories::in_memory().await;
        let (context, _status) = research_finalizer_context(
            db.clone(),
            PathBuf::from("/tmp/missing-research"),
            crate::infra::fs::MockFsClient::new(),
            MockGitClient::new(),
            "missing-research",
        );

        // Act
        archive_research_diff(&context).await;
        let archived_diff = db
            .sessions()
            .load_session_archived_diff("missing-research")
            .await
            .expect("archive lookup should succeed");

        // Assert
        assert_eq!(archived_diff, None);
    }

    #[tokio::test]
    async fn research_diff_archival_tolerates_base_branch_lookup_failure() {
        // Arrange
        let (db, pool) = AppRepositories::in_memory_with_pool().await;
        pool.close().await;
        let (context, _status) = research_finalizer_context(
            db,
            PathBuf::from("/tmp/research-session"),
            crate::infra::fs::MockFsClient::new(),
            MockGitClient::new(),
            "research-session",
        );

        // Act
        archive_research_diff(&context)
            .with_subscriber(crate::test_support::TestSubscriber)
            .await;

        // Assert
        assert!(pool.is_closed());
    }

    #[tokio::test]
    async fn research_diff_archival_tolerates_git_failure() {
        // Arrange
        let db = AppRepositories::in_memory().await;
        insert_research_session(&db, "research-session").await;
        let mut git_client = MockGitClient::new();
        git_client.expect_diff().times(1).returning(|_, _| {
            Box::pin(async { Err(GitError::OutputParse("diff failed".to_string())) })
        });
        let (context, _status) = research_finalizer_context(
            db.clone(),
            PathBuf::from("/tmp/research-session"),
            crate::infra::fs::MockFsClient::new(),
            git_client,
            "research-session",
        );

        // Act
        archive_research_diff(&context)
            .with_subscriber(crate::test_support::TestSubscriber)
            .await;
        let archived_diff = db
            .sessions()
            .load_session_archived_diff("research-session")
            .await
            .expect("archive lookup should succeed");

        // Assert
        assert_eq!(archived_diff, None);
    }

    #[tokio::test]
    async fn research_diff_archival_tolerates_persistence_failure() {
        // Arrange
        let (db, pool) = AppRepositories::in_memory_with_pool().await;
        insert_research_session(&db, "research-session").await;
        let mut git_client = MockGitClient::new();
        git_client.expect_diff().times(1).returning({
            let pool = pool.clone();

            move |_, _| {
                let pool = pool.clone();

                Box::pin(async move {
                    pool.close().await;

                    Ok("diff --git a/policy.txt b/policy.txt\n+write\n".to_string())
                })
            }
        });
        let (context, _status) = research_finalizer_context(
            db,
            PathBuf::from("/tmp/research-session"),
            crate::infra::fs::MockFsClient::new(),
            git_client,
            "research-session",
        );

        // Act
        archive_research_diff(&context)
            .with_subscriber(crate::test_support::TestSubscriber)
            .await;

        // Assert
        assert!(pool.is_closed());
    }

    #[tokio::test]
    async fn test_auto_push_rechecks_queued_rebase_after_waiting_for_branch_lock() {
        // Arrange
        let db = AppRepositories::in_memory().await;
        let project_id = db
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to upsert project");
        db.sessions()
            .insert_session(
                "session-id",
                "gemini-3.6-flash",
                "main",
                "InProgress",
                project_id,
            )
            .await
            .expect("failed to insert session");
        let branch_operation_lock = Arc::new(tokio::sync::Mutex::new(()));
        let enqueue_guard = Arc::clone(&branch_operation_lock).lock_owned().await;
        let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
        let mut mock_git_client = MockGitClient::new();
        mock_git_client
            .expect_push_current_branch_to_remote_branch()
            .never();
        let context = Arc::new(PostTurnContext {
            app_event_tx,
            branch_operation_lock,
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::infra::clock::RealClock),
            db: db.clone(),
            folder: PathBuf::new(),
            git_client: Arc::new(mock_git_client),
            one_shot_client: Arc::new(MockOneShotClient::new()),
            queued_messages: Arc::new(Mutex::new(VecDeque::new())),
            review_request_client: Arc::new(forge::MockReviewRequestClient::new()),
            session_update_versions: Arc::default(),
            session_id: "session-id".into(),
            transcript: Arc::new(Mutex::new(SessionTranscript::default())),
        });
        let auto_push_task = {
            let context = Arc::clone(&context);

            tokio::spawn(async move {
                start_published_branch_auto_push(
                    &context,
                    TurnMetadata {
                        published_upstream_ref: Some("origin/wt/session-id".to_string()),
                        review_comment_thread_ids: Vec::new(),
                        session_agent: AgentSelection::new(
                            AgentKind::Antigravity,
                            AgentModel::Gemini36Flash,
                        ),
                    },
                    None,
                    None,
                    Vec::new(),
                )
                .await;
            })
        };
        db.operations()
            .insert_session_operation("queued-sync", "session-id", "rebase")
            .await
            .expect("failed to insert queued sync");

        // Act
        drop(enqueue_guard);
        auto_push_task.await.expect("auto-push task should join");

        // Assert
        assert!(
            app_event_rx.try_recv().is_err(),
            "the queued sync should retain publish ownership"
        );
    }
}
