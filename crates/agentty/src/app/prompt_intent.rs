use std::fmt::Write as _;
use std::path::PathBuf;

use ag_forge::{ReviewCommentSnapshot, ReviewCommentThread};
use tracing::warn;

use crate::app::task::{PromptImagePasteTaskInput, TaskService};
use crate::app::{App, AppError, ReviewCacheEntry};
use crate::domain::agent::{AgentSelection, ReasoningLevel, ResponseStyle, SpeedMode};
use crate::domain::composer::PromptAttachment;
use crate::domain::permission::PermissionMode;
use crate::domain::personality::PersonalitySummary;
use crate::domain::review;
use crate::domain::session::{SessionId, Status};
use crate::domain::transcript_notice::TranscriptNotice;
use crate::domain::transient_message::{
    TransientMessage, TransientMessageAnchor, TransientMessageBody, TransientMessageLifecycle,
    TransientMessageSlot,
};
use crate::domain::turn_prompt::{TurnPrompt, TurnPromptAttachment, TurnPromptTextSource};
use crate::presentation::app_mode::{AppMode, ReviewCommentSelection};
use crate::presentation::prompt::{PendingImagePaste, insert_prompt_local_image_at};

/// Maximum automatic focused-review remediation turns per user prompt.
pub(crate) const MAX_AUTO_ADDRESS_REVIEW_ITERATIONS: u8 = 3;
/// Checked-in prompt template submitted from the review-comments page.
const RESOLVE_REVIEW_COMMENT_PROMPT_TEMPLATE: &str =
    include_str!("template/resolve_review_comment_prompt.md");

/// Presentation navigation requested after a review-comment resolution attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ReviewCommentResolutionOutcome {
    /// Keep the review-comment page open because no reply was enqueued.
    KeepReviewComments,
    /// Show the session that accepted the review-comment reply.
    ShowSession { session_id: SessionId },
}

/// Typed prompt submission emitted by the presentation boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PromptSubmission {
    /// Structured user input drained from the prompt composer.
    pub(crate) prompt: TurnPrompt,
    /// Stable identifier for the active prompt session.
    pub(crate) session_id: SessionId,
    /// Session lifecycle shape used for app-layer submission routing.
    pub(crate) session_mode: PromptSessionMode,
}

/// Typed cancellation request emitted by the presentation boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PromptCancellation {
    /// Stable identifier for the active prompt session.
    pub(crate) session_id: SessionId,
    /// Session lifecycle shape used for app-layer cancellation routing.
    pub(crate) session_mode: PromptSessionMode,
}

/// Session lifecycle shape used by prompt submission and cancellation routing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PromptSessionMode {
    /// Existing session receiving a follow-up reply.
    Existing,
    /// New non-draft session that can be deleted when prompt composition is
    /// canceled.
    NewDeletable,
    /// Draft-mode session that stages prompt text instead of starting a turn.
    NewDraft,
    /// New non-draft session that should be preserved on cancel because it
    /// has staged drafts.
    NewRegular,
}

/// Presentation navigation requested after one app-layer prompt workflow.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum PromptWorkflowOutcome {
    /// Keep the prompt composer open because no submit action was performed.
    KeepPrompt,
    /// Return to the active session chat view.
    ShowSession { session_id: SessionId },
    /// Return to the top-level session list after deleting a blank draft.
    ShowSessionList,
}

/// Presentation action requested after executing `/apply`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum PromptApplyOutcome {
    /// Clear the accepted slash command and keep the composer open.
    ClearComposer,
    /// Preserve the slash command for correction or retry.
    KeepComposer,
    /// Clear the composer and show the session chat view.
    ShowSession { session_id: SessionId },
}

/// Clipboard-image capture request emitted from a prompt composer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PromptImagePaste {
    /// One-based image placeholder number allocated by presentation state.
    pub(crate) attachment_number: usize,
    /// Session that owns the prompt composer.
    pub(crate) session_id: SessionId,
}

/// Completed clipboard-image capture ready for stale-safe reducer
/// application.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PromptImagePasteUpdate {
    /// Request id returned when the capture started.
    pub(crate) request_id: u64,
    /// Persisted PNG path or normalized user-facing failure text.
    pub(crate) result: Result<PathBuf, String>,
    /// Session whose prompt composer requested the paste.
    pub(crate) session_id: SessionId,
}

impl App {
    /// Submits one agent turn for the selected forge review comments.
    ///
    /// Returns a navigation effect that shows the session only when at least
    /// one actionable comment was rendered and its worker accepted the reply.
    pub(crate) async fn resolve_session_review_comments(
        &mut self,
        session_id: &SessionId,
        snapshot: &ReviewCommentSnapshot,
        selections: &[ReviewCommentSelection],
    ) -> ReviewCommentResolutionOutcome {
        let Some(session_index) = self
            .sessions
            .sessions()
            .iter()
            .position(|session| session.id == *session_id)
        else {
            return ReviewCommentResolutionOutcome::KeepReviewComments;
        };
        if !self.sessions.sessions()[session_index].allows_review_comment_reply() {
            return ReviewCommentResolutionOutcome::KeepReviewComments;
        }

        let Some((prompt, thread_ids)) = build_resolve_review_comment_prompt(snapshot, selections)
        else {
            return ReviewCommentResolutionOutcome::KeepReviewComments;
        };

        let is_queued = self.sessions.sessions()[session_index].queues_review_comment_reply();
        let comment_count = thread_ids.len();
        let enqueued = self
            .sessions
            .reply_to_review_comments(&self.services, session_id, prompt, thread_ids)
            .await;
        if !enqueued {
            return ReviewCommentResolutionOutcome::KeepReviewComments;
        }
        if is_queued {
            return ReviewCommentResolutionOutcome::ShowSession {
                session_id: session_id.clone(),
            };
        }
        self.clear_review_output(session_id.as_str());
        self.clear_diff_comment_progress(session_id);

        // Reply enqueueing cannot reorder the exclusively borrowed session
        // state, so the validated index remains stable across the
        // awaited operation.
        let session = &mut self.sessions.sessions_mut()[session_index];
        session.transient_messages.upsert(TransientMessage {
            anchor: TransientMessageAnchor::Tail,
            body: TransientMessageBody::Loading(review_comment_resolution_loading_text(
                comment_count,
            )),
            lifecycle: TransientMessageLifecycle::UntilResolved,
            slot: TransientMessageSlot::ReviewCommentResolution,
            turn_position: None,
        });

        ReviewCommentResolutionOutcome::ShowSession {
            session_id: session_id.clone(),
        }
    }

    /// Routes one presentation-owned prompt submission through the matching
    /// session workflow and returns the requested navigation effect.
    pub(crate) async fn submit_prompt(
        &mut self,
        submission: PromptSubmission,
    ) -> PromptWorkflowOutcome {
        let PromptSubmission {
            prompt,
            session_id,
            session_mode,
        } = submission;
        if prompt.is_empty() {
            return PromptWorkflowOutcome::KeepPrompt;
        }

        if session_mode == PromptSessionMode::NewDraft
            && self
                .services
                .db()
                .sessions()
                .load_session_preparation(&session_id)
                .await
                .is_ok_and(|preparation| preparation.is_some_and(|row| row.prompt.is_some()))
        {
            self.append_output_for_session(
                &session_id,
                &TranscriptNotice::Error
                    .format("The staged prompt is already waiting for workspace setup"),
            )
            .await;
            return PromptWorkflowOutcome::KeepPrompt;
        }
        if session_mode != PromptSessionMode::NewDraft {
            match self.queue_preparation_prompt(&session_id, &prompt).await {
                Ok(true) => return PromptWorkflowOutcome::ShowSession { session_id },
                Ok(false) => {}
                Err(error) => {
                    self.append_output_for_session(
                        &session_id,
                        &TranscriptNotice::Error.format(error),
                    )
                    .await;
                    return PromptWorkflowOutcome::KeepPrompt;
                }
            }
        }
        self.auto_address_review_iterations.remove(&session_id);
        self.submit_turn_prompt(session_id.clone(), session_mode, prompt)
            .await;

        PromptWorkflowOutcome::ShowSession { session_id }
    }

    /// Starts one background clipboard-image capture and returns the request
    /// id the presentation-owned composer tracks while the capture runs.
    ///
    /// The result arrives as [`crate::app::AppEvent::PromptImagePasted`] and is
    /// applied by [`App::apply_prompt_image_paste_update`]. The capture is
    /// tracked as cleanup work so graceful shutdown waits for it and removes
    /// an image that was never applied.
    pub(crate) fn start_prompt_image_paste(&self, request: PromptImagePaste) -> u64 {
        let (request_id, capture_task) =
            TaskService::spawn_prompt_image_paste_task(PromptImagePasteTaskInput {
                app_event_tx: self.services.event_sender(),
                attachment_number: request.attachment_number,
                clipboard_image_client: self.services.clipboard_image_client(),
                session_id: request.session_id,
            });
        self.services.track_cleanup_task(capture_task);

        request_id
    }

    /// Applies one completed clipboard-image capture to the prompt composer
    /// that is still waiting for it.
    ///
    /// The placeholder lands where the paste shortcut was pressed, shifted
    /// across edits made while the capture ran. Captures finishing after
    /// their composer was submitted, canceled, or replaced remove their
    /// orphaned PNG instead of inserting a placeholder. Capture failures are
    /// reported in the session transcript.
    pub(crate) async fn apply_prompt_image_paste_update(&mut self, update: PromptImagePasteUpdate) {
        let PromptImagePasteUpdate {
            request_id,
            result,
            session_id,
        } = update;
        let pending_image_paste = self.take_pending_prompt_image_paste(&session_id, request_id);

        match (result, pending_image_paste) {
            (Ok(local_image_path), Some(pending_image_paste)) => {
                let unreachable_attachments = self
                    .insert_prompt_image_placeholder(local_image_path, pending_image_paste.anchor);
                self.cleanup_prompt_attachments(unreachable_attachments)
                    .await;
            }
            (Ok(local_image_path), None) => {
                let orphaned_prompt = TurnPrompt {
                    attachments: vec![TurnPromptAttachment {
                        local_image_path,
                        placeholder: String::new(),
                    }],
                    text: String::new(),
                    text_source: TurnPromptTextSource::UserPrompt,
                };
                self.cleanup_prompt_attachment_files(&orphaned_prompt).await;
            }
            (Err(message), _) => {
                self.append_prompt_status_line(
                    session_id.as_str(),
                    TranscriptNotice::PasteImageError,
                    &message,
                )
                .await;
            }
        }
    }

    /// Inserts one persisted image placeholder into the active prompt
    /// composer at `anchor`, or at the cursor when no anchor survived, and
    /// returns archived attachments that are no longer reachable through
    /// undo history.
    pub(crate) fn insert_prompt_image_placeholder(
        &mut self,
        local_image_path: PathBuf,
        anchor: Option<usize>,
    ) -> Vec<PromptAttachment> {
        let AppMode::Prompt {
            at_mention_state,
            attachment_state,
            history_state,
            input,
            slash_state,
            ..
        } = &mut self.mode
        else {
            return Vec::new();
        };

        let anchor = anchor.unwrap_or(input.cursor);
        insert_prompt_local_image_at(
            attachment_state,
            history_state,
            input,
            slash_state,
            local_image_path,
            anchor,
        );
        *at_mention_state = None;

        attachment_state.prune_unreachable(input)
    }

    /// Clears and returns the composer's pending capture when it matches
    /// `request_id`, meaning the completion still belongs to that composer.
    fn take_pending_prompt_image_paste(
        &mut self,
        session_id: &SessionId,
        request_id: u64,
    ) -> Option<PendingImagePaste> {
        let AppMode::Prompt {
            attachment_state,
            session_id: mode_session_id,
            ..
        } = &mut self.mode
        else {
            return None;
        };
        if mode_session_id != session_id
            || attachment_state
                .pending_image_paste
                .is_none_or(|pending_image_paste| pending_image_paste.request_id != request_id)
        {
            return None;
        }

        attachment_state.pending_image_paste.take()
    }

    /// Removes image files whose attachment identities are no longer
    /// reachable through the presentation-owned prompt composer.
    pub(crate) async fn cleanup_prompt_attachments(&self, attachments: Vec<PromptAttachment>) {
        if attachments.is_empty() {
            return;
        }

        let attachments = attachments
            .into_iter()
            .map(|attachment| TurnPromptAttachment {
                local_image_path: attachment.local_image_path,
                placeholder: attachment.placeholder,
            })
            .collect();
        let prompt = TurnPrompt {
            attachments,
            text: String::new(),
            text_source: TurnPromptTextSource::UserPrompt,
        };

        self.cleanup_prompt_attachment_files(&prompt).await;
    }

    /// Cancels one presentation-owned prompt and returns the requested
    /// navigation effect.
    pub(crate) async fn cancel_prompt(
        &mut self,
        cancellation: PromptCancellation,
    ) -> PromptWorkflowOutcome {
        if cancellation.session_mode == PromptSessionMode::NewDeletable {
            self.delete_selected_session_deferred_cleanup().await;

            return PromptWorkflowOutcome::ShowSessionList;
        }

        PromptWorkflowOutcome::ShowSession {
            session_id: cancellation.session_id,
        }
    }

    /// Returns whether cached focused-review text contains actionable
    /// suggestions for one session.
    pub(crate) fn prompt_apply_command_is_available_for_session(&self, session_id: &str) -> bool {
        let Some(ReviewCacheEntry::Ready { text, .. }) = self.review_cache.get(session_id) else {
            return false;
        };

        review::has_actionable_review_suggestions(Some(text))
    }

    /// Persists one slash-selected model change and logs any failure with
    /// session context.
    pub(crate) async fn update_prompt_session_model(
        &mut self,
        session_id: &SessionId,
        selected_agent: AgentSelection,
    ) {
        let should_disable_fast_mode = self
            .sessions
            .sessions()
            .iter()
            .find(|session| session.id == *session_id)
            .is_some_and(|session| {
                session.speed_mode == SpeedMode::Fast && !selected_agent.supports_fast_mode()
            });
        if should_disable_fast_mode
            && let Err(error) = self
                .set_session_speed_mode(session_id, SpeedMode::Normal)
                .await
        {
            let agent_kind = selected_agent.kind();
            let agent_model = selected_agent.model().as_str();
            warn!(
                session_id = %session_id,
                agent = %agent_kind,
                model = %agent_model,
                error = %error,
                "failed to disable fast mode before switching to an incompatible model"
            );

            return;
        }

        if let Err(error) = self.set_session_model(session_id, selected_agent).await {
            warn!(
                session_id = %session_id,
                agent = %selected_agent.kind(),
                model = %selected_agent.model().as_str(),
                error = %error,
                "failed to switch session model from prompt slash command"
            );
        }
    }

    /// Loads picker metadata from the targeted session worktree.
    pub(crate) async fn list_prompt_personalities(
        &self,
        session_id: &SessionId,
    ) -> Vec<PersonalitySummary> {
        let Some(folder) = self
            .sessions
            .sessions()
            .iter()
            .find(|session| session.id == *session_id)
            .map(|session| session.folder.clone())
        else {
            return Vec::new();
        };

        self.services
            .personality_catalog_client()
            .list_summaries(folder)
            .await
    }

    /// Persists one slash-selected personality and appends visible feedback.
    pub(crate) async fn update_prompt_session_personality(
        &mut self,
        session_id: &SessionId,
        personality: Option<PersonalitySummary>,
    ) {
        let personality_id = personality
            .as_ref()
            .map(|personality| personality.id.clone());
        if let Err(error) = self
            .set_session_personality(session_id, personality_id)
            .await
        {
            warn!(
                session_id = %session_id,
                error = %error,
                "failed to update session personality from prompt slash command"
            );

            return;
        }

        let message = personality.map_or_else(
            || "Personality cleared.".to_string(),
            |personality| format!("Personality set to *{}*.", personality.name),
        );
        self.append_prompt_status_line(session_id, TranscriptNotice::Personality, &message)
            .await;
    }

    /// Persists one slash-selected reasoning level and logs any failure with
    /// session context.
    pub(crate) async fn update_prompt_session_reasoning_level(
        &mut self,
        session_id: &SessionId,
        reasoning_level: ReasoningLevel,
    ) {
        if let Err(error) = self
            .set_session_reasoning_level(session_id, reasoning_level)
            .await
        {
            warn!(
                session_id = %session_id,
                reasoning_level = ?reasoning_level,
                error = %error,
                "failed to update session reasoning level from prompt slash command"
            );
        }
    }

    /// Persists one slash-selected response style and logs any failure with
    /// session context.
    pub(crate) async fn update_prompt_session_response_style(
        &mut self,
        session_id: &SessionId,
        response_style: ResponseStyle,
    ) {
        if let Err(error) = self
            .set_session_response_style(session_id, response_style)
            .await
        {
            warn!(
                session_id = %session_id,
                response_style = ?response_style,
                error = %error,
                "failed to update session response style from prompt slash command"
            );
        }
    }

    /// Persists one prompt-selected provider permission mode.
    ///
    /// # Errors
    /// Returns an error when the session is missing or persistence fails.
    pub(crate) async fn update_prompt_session_permission_mode(
        &mut self,
        session_id: &SessionId,
        permission_mode: PermissionMode,
    ) -> Result<(), AppError> {
        let result = self
            .set_session_permission_mode(session_id, permission_mode)
            .await;
        if let Err(error) = &result {
            warn!(
                session_id = %session_id,
                permission_mode = ?permission_mode,
                error = %error,
                "failed to update session permission mode from prompt shortcut"
            );
        }

        if result.is_ok() {
            self.auto_address_review_iterations.remove(session_id);
        }

        result
    }

    /// Starts bounded `/apply`-equivalent turns for newly ready focused
    /// reviews whose session mode enables automatic remediation.
    pub(crate) fn auto_address_focused_reviews(&mut self, ready_session_ids: Vec<SessionId>) {
        for session_id in ready_session_ids {
            let Some(session_index) = self
                .sessions
                .sessions()
                .iter()
                .position(|session| session.id == session_id)
            else {
                continue;
            };
            if self.sessions.sessions()[session_index].permission_mode
                != PermissionMode::AutoEditAddressComments
            {
                continue;
            }

            let completed_iterations = self
                .auto_address_review_iterations
                .get(&session_id)
                .copied()
                .unwrap_or(0);
            if completed_iterations >= MAX_AUTO_ADDRESS_REVIEW_ITERATIONS {
                continue;
            }

            let Some((cached_hash, suggestions)) =
                self.review_cache
                    .get(&session_id)
                    .and_then(|entry| match entry {
                        ReviewCacheEntry::Ready {
                            diff_hash, text, ..
                        } => review::review_suggestions(text)
                            .map(|suggestions| (*diff_hash, suggestions)),
                        ReviewCacheEntry::Loading { .. }
                        | ReviewCacheEntry::Failed { .. }
                        | ReviewCacheEntry::Suppressed => None,
                    })
            else {
                continue;
            };
            self.start_auto_apply_review_diff_load(&session_id, cached_hash, suggestions);
        }
    }

    /// Persists one slash-selected response-speed preference and logs any
    /// failure with session context.
    pub(crate) async fn update_prompt_session_speed_mode(
        &mut self,
        session_id: &SessionId,
        speed_mode: SpeedMode,
    ) {
        if speed_mode == SpeedMode::Fast {
            let fast_agent = self
                .sessions
                .sessions()
                .iter()
                .find(|session| session.id == *session_id)
                .and_then(|session| {
                    let fast_agent = session.agent.compatible_with_speed_mode(SpeedMode::Fast);

                    (fast_agent != session.agent).then_some(fast_agent)
                });

            if let Some(fast_agent) = fast_agent {
                if let Err(error) = self
                    .sessions
                    .set_session_model_for_speed_mode(
                        &self.services,
                        session_id.as_str(),
                        fast_agent,
                    )
                    .await
                {
                    let agent_kind = fast_agent.kind();
                    let agent_model = fast_agent.model().as_str();
                    warn!(
                        session_id = %session_id,
                        agent = %agent_kind,
                        model = %agent_model,
                        error = %error,
                        "failed to switch session model before enabling fast mode"
                    );

                    return;
                }

                self.process_pending_app_events().await;
            }
        }

        if let Err(error) = self.set_session_speed_mode(session_id, speed_mode).await {
            warn!(
                session_id = %session_id,
                speed_mode = ?speed_mode,
                error = %error,
                "failed to update session speed mode from prompt slash command"
            );
        }
    }

    /// Handles `/apply` by extracting suggestions from the focused review and
    /// submitting them as a verification-gated prompt to the agent.
    pub(crate) async fn apply_focused_review(
        &mut self,
        session_id: &SessionId,
        session_index: usize,
    ) -> PromptApplyOutcome {
        let Some(session_status) = self.session_at(session_index).map(|session| session.status)
        else {
            return PromptApplyOutcome::KeepComposer;
        };

        if session_status != Status::Review {
            self.append_prompt_status_line(
                session_id,
                TranscriptNotice::Apply,
                "Apply is only available after a focused review completes (session status must be \
                 Review).",
            )
            .await;

            return PromptApplyOutcome::ClearComposer;
        }

        let (cached_hash, cached_text) =
            if let Some(ReviewCacheEntry::Ready {
                diff_hash, text, ..
            }) = self.review_cache.get(session_id.as_str())
            {
                (*diff_hash, text.clone())
            } else {
                self.append_prompt_status_line(
                    session_id,
                    TranscriptNotice::Apply,
                    "No actionable suggestions available. Run a focused review first (f key).",
                )
                .await;

                return PromptApplyOutcome::ClearComposer;
            };

        let Some(suggestions) = review::review_suggestions(&cached_text) else {
            self.append_prompt_status_line(
                session_id,
                TranscriptNotice::Apply,
                "No actionable suggestions found in the current review.",
            )
            .await;

            return PromptApplyOutcome::KeepComposer;
        };

        if !self.start_apply_review_diff_load(session_id, cached_hash, suggestions) {
            self.append_prompt_status_line(
                session_id,
                TranscriptNotice::Apply,
                "An /apply worktree diff check is already running or unavailable; try again \
                 shortly.",
            )
            .await;

            return PromptApplyOutcome::ClearComposer;
        }

        PromptApplyOutcome::ShowSession {
            session_id: session_id.clone(),
        }
    }

    /// Routes one prepared turn prompt through the lifecycle path for the
    /// active prompt session.
    async fn submit_turn_prompt(
        &mut self,
        session_id: SessionId,
        session_mode: PromptSessionMode,
        prompt: TurnPrompt,
    ) {
        if session_mode == PromptSessionMode::NewDraft {
            if let Err(error) = self.stage_draft_message(&session_id, prompt).await {
                self.append_output_for_session(&session_id, &TranscriptNotice::Error.format(error))
                    .await;
            }
        } else if session_mode != PromptSessionMode::Existing {
            if let Err(error) = self.start_session(&session_id, prompt).await {
                self.append_output_for_session(&session_id, &TranscriptNotice::Error.format(error))
                    .await;
            }
        } else if self.session_queues_messages(&session_id) {
            if let Err(error) = self.enqueue_message(&session_id, prompt) {
                self.append_output_for_session(
                    &session_id,
                    &TranscriptNotice::QueueError.format(error),
                )
                .await;
            }
        } else {
            self.reply(&session_id, prompt).await;
        }
    }

    /// Returns whether the targeted session is running a turn or rebase, used
    /// to route submissions into the in-memory message queue.
    fn session_queues_messages(&self, session_id: &str) -> bool {
        self.sessions
            .sessions()
            .iter()
            .find(|session| session.id == session_id)
            .is_some_and(|session| matches!(session.status, Status::InProgress | Status::Rebasing))
    }

    /// Appends one prompt-workflow status line to the target session
    /// transcript.
    pub(crate) async fn append_prompt_status_line(
        &self,
        session_id: &str,
        notice: TranscriptNotice,
        message: &str,
    ) {
        self.append_output_for_session(session_id, &notice.format(message))
            .await;
    }
}

/// Formats the in-progress label for one accepted review-comment batch.
pub(crate) fn review_comment_resolution_loading_text(comment_count: usize) -> String {
    let noun = if comment_count == 1 {
        "review comment"
    } else {
        "review comments"
    };

    format!("Resolving {comment_count} {noun}...")
}

/// Builds an agent-facing review-comment prompt and its forge thread
/// allowlist.
///
/// Resolved threads are excluded. Standalone discussion comments are
/// read-only because they have no forge-side thread identifier.
pub(crate) fn build_resolve_review_comment_prompt(
    snapshot: &ReviewCommentSnapshot,
    selections: &[ReviewCommentSelection],
) -> Option<(TurnPrompt, Vec<String>)> {
    let threads = selected_review_comment_threads(snapshot, selections);
    if threads.is_empty() {
        return None;
    }

    let mut review_comments = String::new();
    for thread in &threads {
        append_review_thread_prompt(&mut review_comments, thread);
    }

    let thread_ids = threads
        .into_iter()
        .map(|thread| thread.id.clone())
        .collect::<Vec<_>>();
    let review_comments = review_comments.trim_end();
    let fence = ag_protocol::diff_fence(review_comments);
    let fenced_review_comments = format!("{fence}text\n{review_comments}\n{fence}");
    let prompt = RESOLVE_REVIEW_COMMENT_PROMPT_TEMPLATE
        .trim_end()
        .replace("{{ fenced_review_comments }}", &fenced_review_comments);

    Some((TurnPrompt::from_agent_data(prompt), thread_ids))
}

/// Returns the actionable inline threads selected for a turn.
fn selected_review_comment_threads<'a>(
    snapshot: &'a ReviewCommentSnapshot,
    selections: &[ReviewCommentSelection],
) -> Vec<&'a ReviewCommentThread> {
    snapshot
        .threads
        .iter()
        .filter(|thread| thread.is_actionable())
        .filter_map(|thread| {
            selections
                .iter()
                .find(|selection| selection.thread_id == thread.id)
                .map(|_| thread)
        })
        .collect()
}

/// Appends one thread's stable identifier, anchor, and conversation text.
fn append_review_thread_prompt(review_comments: &mut String, thread: &ReviewCommentThread) {
    let _ = writeln!(review_comments, "Thread ID: {}", thread.id);
    let _ = writeln!(review_comments, "Path: {}", thread.path);
    if thread.is_outdated == Some(true) {
        let _ = writeln!(
            review_comments,
            "Anchor status: outdated; inspect the current file instead of trusting the line anchor"
        );
    }
    let _ = writeln!(
        review_comments,
        "Anchor: {:?}, start line: {}, end line: {}",
        thread.anchor_side,
        thread
            .start_line
            .map_or_else(|| "none".to_string(), |line| line.to_string()),
        thread
            .line
            .map_or_else(|| "none".to_string(), |line| line.to_string())
    );
    for comment in &thread.comments {
        let _ = writeln!(
            review_comments,
            "Comment by {}:\n{}",
            comment.author, comment.body
        );
    }
    review_comments.push('\n');
}

#[cfg(test)]
#[path = "prompt_intent_test.rs"]
mod tests;
