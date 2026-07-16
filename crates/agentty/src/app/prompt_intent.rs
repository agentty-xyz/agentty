use std::path::PathBuf;

use ag_agent as agent;
use tracing::warn;

use crate::app::{App, ReviewCacheEntry, diff_content_hash};
use crate::domain::agent::{AgentKind, ReasoningLevel};
use crate::domain::composer::PromptAttachment;
use crate::domain::review;
use crate::domain::session::{SessionId, Status};
use crate::domain::transcript_notice::TranscriptNotice;
use crate::domain::turn_prompt::{TurnPrompt, TurnPromptAttachment, TurnPromptTextSource};
use crate::infra::clipboard_image;
use crate::presentation::app_mode::AppMode;
use crate::presentation::prompt::{
    PromptSlashStage, PromptSuggestionSelection, drain_prompt_submission,
    insert_prompt_local_image, resolve_prompt_slash_selection,
};

/// Checked-in prompt template submitted by the `/apply` slash command.
const APPLY_REVIEW_PROMPT_TEMPLATE: &str = include_str!("template/apply_review_prompt.md");

/// App-layer prompt intent context derived from prompt-mode runtime state.
///
/// Runtime key handling constructs this snapshot when a key maps to an
/// application workflow. The app layer then owns the execution decision:
/// prompt submission routing, slash-command selection, cancellation cleanup,
/// and view restoration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PromptIntentContext {
    /// Active prompt input shape used for app-layer submit/cancel routing.
    pub(crate) input_mode: PromptIntentInputMode,
    /// Session transcript scroll offset to restore when returning to view.
    pub(crate) scroll_offset: Option<u16>,
    /// Stable identifier for the active prompt session.
    pub(crate) session_id: SessionId,
    /// Current list index for the active prompt session.
    pub(crate) session_index: usize,
    /// Session lifecycle shape used for app-layer submit/cancel routing.
    pub(crate) session_mode: PromptIntentSessionMode,
}

impl PromptIntentContext {
    /// Returns whether `Esc` should delete the blank backing session instead
    /// of restoring session view.
    fn can_delete_on_cancel(&self) -> bool {
        self.session_mode == PromptIntentSessionMode::NewDeletable
    }

    /// Returns whether this prompt belongs to a draft session still staging
    /// messages.
    fn is_draft_session(&self) -> bool {
        self.session_mode == PromptIntentSessionMode::NewDraft
    }

    /// Returns whether this prompt belongs to a session that has not started
    /// yet.
    fn is_new_session(&self) -> bool {
        self.session_mode != PromptIntentSessionMode::Existing
    }

    /// Returns whether the active prompt input currently represents a slash
    /// command.
    fn is_slash_command(&self) -> bool {
        self.input_mode == PromptIntentInputMode::SlashCommand
    }
}

/// Active prompt input shape used by app-layer prompt intent routing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PromptIntentInputMode {
    /// Prompt text starts with a slash-command prefix.
    SlashCommand,
    /// Prompt text is normal user input.
    Text,
}

/// Session lifecycle shape used by app-layer prompt intent routing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PromptIntentSessionMode {
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

impl App {
    /// Handles the submit intent emitted by prompt-mode key routing.
    ///
    /// Slash-command submissions are resolved inside the app layer. Text
    /// submissions drain the prompt composer, choose the correct session
    /// workflow, and return to session view after a non-empty prompt.
    pub(crate) async fn handle_prompt_submit_intent(&mut self, context: &PromptIntentContext) {
        if context.is_slash_command() {
            self.handle_prompt_slash_submit_intent(context).await;

            return;
        }

        let archived_prompt = self.archived_prompt_attachments();
        let prompt = self.take_submitted_turn_prompt();
        if let Some(archived_prompt) = archived_prompt {
            self.cleanup_prompt_attachment_files(&archived_prompt).await;
        }
        if prompt.is_empty() {
            return;
        }

        self.submit_turn_prompt_for_context(context, prompt).await;
        self.restore_prompt_session_view(context);
    }

    /// Executes the active slash-command selection for prompt mode.
    pub(crate) async fn handle_prompt_slash_submit_intent(
        &mut self,
        context: &PromptIntentContext,
    ) {
        let session_agent_kind = self
            .session_at(context.session_index)
            .map_or(AgentKind::Codex, |session| session.agent.kind());
        let selection = match &self.mode {
            AppMode::Prompt {
                input, slash_state, ..
            } => resolve_prompt_slash_selection(
                input.text(),
                slash_state,
                session_agent_kind,
                self.prompt_apply_command_is_available_for_session(&context.session_id),
            ),
            _ => None,
        };

        match selection {
            Some(PromptSuggestionSelection::Command("/apply")) => {
                if self.handle_apply_prompt_command(context).await {
                    self.reset_prompt_slash_input().await;
                } else {
                    self.reset_prompt_slash_state();
                }
            }
            Some(PromptSuggestionSelection::Command("/reasoning")) => {
                let selected_reasoning_level = self
                    .session_at(context.session_index)
                    .map_or(self.settings.reasoning_level, |session| {
                        session.effective_reasoning_level()
                    });
                let selected_index = ReasoningLevel::ALL
                    .iter()
                    .position(|level| *level == selected_reasoning_level)
                    .unwrap_or(0);

                if let AppMode::Prompt { slash_state, .. } = &mut self.mode {
                    slash_state.stage = PromptSlashStage::Reasoning;
                    slash_state.selected_agent = None;
                    slash_state.selected_index = selected_index;
                }
            }
            Some(PromptSuggestionSelection::Command(_)) => {
                if let AppMode::Prompt { slash_state, .. } = &mut self.mode {
                    slash_state.stage = PromptSlashStage::Agent;
                    slash_state.selected_agent = None;
                    slash_state.selected_index = 0;
                }
            }
            Some(PromptSuggestionSelection::Agent(selected_agent)) => {
                if let AppMode::Prompt { slash_state, .. } = &mut self.mode {
                    slash_state.selected_agent = Some(selected_agent);
                    slash_state.stage = PromptSlashStage::Model;
                    slash_state.selected_index = 0;
                }
            }
            Some(PromptSuggestionSelection::Model(selected_agent)) => {
                self.reset_prompt_slash_input().await;
                self.update_prompt_session_model(context, selected_agent)
                    .await;
            }
            Some(PromptSuggestionSelection::Reasoning(reasoning_level)) => {
                self.reset_prompt_slash_input().await;
                self.update_prompt_session_reasoning_level(context, reasoning_level)
                    .await;
            }
            None => {}
        }
    }

    /// Handles the image-paste intent emitted by prompt-mode key routing.
    ///
    /// The app layer owns attachment numbering, clipboard-image service
    /// dispatch, prompt mutation, and user-visible paste errors so runtime
    /// only routes the key intent.
    pub(crate) async fn handle_prompt_image_paste_intent(&mut self, context: &PromptIntentContext) {
        let attachment_number = match &self.mode {
            AppMode::Prompt {
                attachment_state, ..
            } => attachment_state.next_attachment_number,
            _ => return,
        };

        match self
            .services
            .clipboard_image_client()
            .persist_clipboard_image(context.session_id.as_str().to_string(), attachment_number)
            .await
        {
            Ok(persisted_image) => {
                let unreachable_attachments =
                    self.insert_pasted_image_placeholder(persisted_image.local_image_path);
                self.cleanup_prompt_attachments(unreachable_attachments)
                    .await;
            }
            Err(error) => {
                self.append_prompt_status_line(
                    context.session_id.as_str(),
                    TranscriptNotice::PasteImageError,
                    &clipboard_image::normalize_clipboard_image_error(&error),
                )
                .await;
            }
        }
    }

    /// Inserts one persisted image placeholder into the prompt input and
    /// records the attachment metadata in prompt state.
    pub(crate) fn insert_pasted_image_placeholder(
        &mut self,
        local_image_path: PathBuf,
    ) -> Vec<PromptAttachment> {
        if let AppMode::Prompt {
            at_mention_state,
            attachment_state,
            history_state,
            input,
            slash_state,
            ..
        } = &mut self.mode
        {
            insert_prompt_local_image(
                attachment_state,
                history_state,
                input,
                slash_state,
                local_image_path,
            );
            *at_mention_state = None;

            return attachment_state.prune_unreachable(input);
        }

        Vec::new()
    }

    /// Removes image files whose attachment identities are no longer
    /// reachable through the prompt input's bounded undo/redo history.
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

    /// Handles the cancel intent emitted by prompt-mode key routing.
    ///
    /// Slash-command cancellation only clears the slash input. Prompt
    /// cancellation cleans up composer-owned attachments, optionally deletes
    /// a blank backing session, and otherwise restores session view.
    pub(crate) async fn handle_prompt_cancel_intent(&mut self, context: &PromptIntentContext) {
        if context.is_slash_command() {
            self.reset_prompt_slash_input().await;

            return;
        }

        self.cleanup_prompt_attachment_state().await;

        if context.can_delete_on_cancel() {
            self.delete_selected_session_deferred_cleanup().await;
            self.mode = AppMode::List;

            return;
        }

        self.mode = AppMode::View {
            scroll_offset: context.scroll_offset,
            session_id: context.session_id.clone(),
        };
    }

    /// Returns whether the active prompt session has cached focused-review
    /// suggestions that make `/apply` selectable.
    pub(crate) fn prompt_apply_command_is_available(&self) -> bool {
        let Some(session_id) = self.prompt_session_id() else {
            return false;
        };

        self.prompt_apply_command_is_available_for_session(&session_id)
    }

    /// Drains the prompt composer into the structured turn payload sent to
    /// the session workflow.
    ///
    /// Attachments are filtered against the submitted text so manually
    /// deleted `[Image #n]` placeholders do not leave orphaned image inputs in
    /// the final turn payload.
    pub(crate) fn take_submitted_turn_prompt(&mut self) -> TurnPrompt {
        match &mut self.mode {
            AppMode::Prompt {
                attachment_state,
                input,
                ..
            } => {
                let submission = drain_prompt_submission(attachment_state, input);
                let attachments = submission
                    .attachments
                    .into_iter()
                    .map(|attachment| TurnPromptAttachment {
                        local_image_path: attachment.local_image_path,
                        placeholder: attachment.placeholder,
                    })
                    .collect();

                TurnPrompt {
                    attachments,
                    text: submission.text,
                    text_source: TurnPromptTextSource::UserPrompt,
                }
            }
            _ => TurnPrompt::from_text(String::new()),
        }
    }

    /// Builds a cleanup-only prompt for image attachments currently absent
    /// from the editable text but retained for undo.
    fn archived_prompt_attachments(&self) -> Option<TurnPrompt> {
        let AppMode::Prompt {
            attachment_state, ..
        } = &self.mode
        else {
            return None;
        };
        if attachment_state.archived_attachments.is_empty() {
            return None;
        }

        let attachments = attachment_state
            .archived_attachments
            .iter()
            .map(|attachment| TurnPromptAttachment {
                local_image_path: attachment.local_image_path.clone(),
                placeholder: attachment.placeholder.clone(),
            })
            .collect();

        Some(TurnPrompt {
            attachments,
            text: String::new(),
            text_source: TurnPromptTextSource::UserPrompt,
        })
    }

    /// Routes one prepared turn prompt through the lifecycle path for the
    /// active prompt context.
    async fn submit_turn_prompt_for_context(
        &mut self,
        context: &PromptIntentContext,
        prompt: TurnPrompt,
    ) {
        if context.is_draft_session() {
            if let Err(error) = self.stage_draft_message(&context.session_id, prompt).await {
                self.append_output_for_session(
                    &context.session_id,
                    &TranscriptNotice::Error.format(error),
                )
                .await;
            }
        } else if context.is_new_session() {
            if let Err(error) = self.start_session(&context.session_id, prompt).await {
                self.append_output_for_session(
                    &context.session_id,
                    &TranscriptNotice::Error.format(error),
                )
                .await;
            }
        } else if self.session_queues_messages(&context.session_id) {
            if let Err(error) = self.enqueue_message(&context.session_id, prompt) {
                self.append_output_for_session(
                    &context.session_id,
                    &TranscriptNotice::QueueError.format(error),
                )
                .await;
            }
        } else {
            self.reply(&context.session_id, prompt).await;
        }
    }

    /// Restores view mode for the session that owned prompt input.
    fn restore_prompt_session_view(&mut self, context: &PromptIntentContext) {
        self.mode = AppMode::View {
            scroll_offset: None,
            session_id: context.session_id.clone(),
        };
    }

    /// Returns whether the targeted session is running a turn or rebase, used
    /// to route non-slash submissions into the in-memory message queue
    /// instead of the live reply path.
    fn session_queues_messages(&self, session_id: &str) -> bool {
        self.sessions
            .sessions()
            .iter()
            .find(|session| session.id == session_id)
            .is_some_and(|session| matches!(session.status, Status::InProgress | Status::Rebasing))
    }

    /// Clears the slash-command buffer and cleans up attachments removed with
    /// it after one prompt slash action is accepted or canceled.
    async fn reset_prompt_slash_input(&mut self) {
        self.cleanup_prompt_attachment_state().await;

        if let AppMode::Prompt {
            input, slash_state, ..
        } = &mut self.mode
        {
            input.take_text();
            slash_state.reset();
        }
    }

    /// Resets the slash-command menu without clearing the user's input text.
    fn reset_prompt_slash_state(&mut self) {
        if let AppMode::Prompt { slash_state, .. } = &mut self.mode {
            slash_state.reset();
        }
    }

    /// Persists one slash-selected model change and logs any failure with
    /// session context.
    async fn update_prompt_session_model(
        &mut self,
        context: &PromptIntentContext,
        selected_agent: crate::domain::agent::AgentSelection,
    ) {
        if let Err(error) = self
            .set_session_model(&context.session_id, selected_agent)
            .await
        {
            warn!(
                session_id = %context.session_id,
                agent = %selected_agent.kind(),
                model = %selected_agent.model().as_str(),
                error = %error,
                "failed to switch session model from prompt slash command"
            );
        }
    }

    /// Persists one slash-selected reasoning level and logs any failure
    /// with session context.
    async fn update_prompt_session_reasoning_level(
        &mut self,
        context: &PromptIntentContext,
        reasoning_level: ReasoningLevel,
    ) {
        if let Err(error) = self
            .set_session_reasoning_level(&context.session_id, reasoning_level)
            .await
        {
            warn!(
                session_id = %context.session_id,
                reasoning_level = ?reasoning_level,
                error = %error,
                "failed to update session reasoning level from prompt slash command"
            );
        }
    }

    /// Returns the active prompt session id without mutating prompt state.
    fn prompt_session_id(&self) -> Option<SessionId> {
        match &self.mode {
            AppMode::Prompt { session_id, .. } => Some(session_id.clone()),
            _ => None,
        }
    }

    /// Returns whether cached focused-review text contains actionable
    /// suggestions for one session.
    fn prompt_apply_command_is_available_for_session(&self, session_id: &str) -> bool {
        let Some(ReviewCacheEntry::Ready { text, .. }) = self.review_cache.get(session_id) else {
            return false;
        };

        review::has_actionable_review_suggestions(Some(text))
    }

    /// Appends one prompt-mode status line to the session transcript shown
    /// above the composer.
    async fn append_prompt_status_line(
        &self,
        session_id: &str,
        notice: TranscriptNotice,
        message: &str,
    ) {
        self.append_output_for_session(session_id, &notice.format(message))
            .await;
    }

    /// Removes any prompt attachment files still owned by the active composer
    /// and resets attachment state before leaving prompt mode.
    async fn cleanup_prompt_attachment_state(&mut self) {
        let prompt = match &mut self.mode {
            AppMode::Prompt {
                attachment_state, ..
            } => {
                let attachments = attachment_state
                    .attachments
                    .iter()
                    .chain(&attachment_state.archived_attachments)
                    .map(|attachment| TurnPromptAttachment {
                        local_image_path: attachment.local_image_path.clone(),
                        placeholder: attachment.placeholder.clone(),
                    })
                    .collect::<Vec<_>>();
                attachment_state.reset();

                TurnPrompt {
                    attachments,
                    text: String::new(),
                    text_source: TurnPromptTextSource::UserPrompt,
                }
            }
            _ => return,
        };

        self.cleanup_prompt_attachment_files(&prompt).await;
    }

    /// Handles `/apply` by extracting suggestions from the focused review and
    /// submitting them as a verification-gated prompt to the agent.
    ///
    /// Returns `true` when the command consumed the slash-command input. A
    /// validation failure that leaves actionable cached suggestions available
    /// returns `false`, preserving the visible `/apply` text for correction or
    /// retry.
    async fn handle_apply_prompt_command(&mut self, context: &PromptIntentContext) -> bool {
        let Some((session_status, session_folder, base_branch)) =
            self.session_at(context.session_index).map(|session| {
                (
                    session.status,
                    session.folder.clone(),
                    session.base_branch.clone(),
                )
            })
        else {
            return false;
        };

        if session_status != Status::Review {
            self.append_prompt_status_line(
                &context.session_id,
                TranscriptNotice::Apply,
                "Apply is only available after a focused review completes (session status must be \
                 Review).",
            )
            .await;

            return true;
        }

        let (cached_hash, cached_text) = if let Some(ReviewCacheEntry::Ready { diff_hash, text }) =
            self.review_cache.get(context.session_id.as_str())
        {
            (*diff_hash, text.clone())
        } else {
            self.append_prompt_status_line(
                &context.session_id,
                TranscriptNotice::Apply,
                "No actionable suggestions available. Run a focused review first (f key).",
            )
            .await;

            return true;
        };

        let current_diff = match self
            .services
            .git_client()
            .diff(session_folder, base_branch)
            .await
        {
            Ok(diff) => diff,
            Err(err) => {
                self.append_prompt_status_line(
                    &context.session_id,
                    TranscriptNotice::Apply,
                    &format!(
                        "Failed to read worktree diff: {err}. Review cache preserved; try /apply \
                         again."
                    ),
                )
                .await;

                return true;
            }
        };
        let current_hash = diff_content_hash(&current_diff);

        if current_hash != cached_hash {
            self.clear_review_output(context.session_id.as_str());
            self.append_prompt_status_line(
                &context.session_id,
                TranscriptNotice::Apply,
                "Review is stale; the worktree changed since it was generated. Run focused review \
                 again (f key).",
            )
            .await;

            return true;
        }

        let Some(suggestions) = review::review_suggestions(&cached_text) else {
            self.append_prompt_status_line(
                &context.session_id,
                TranscriptNotice::Apply,
                "No actionable suggestions found in the current review.",
            )
            .await;

            return false;
        };

        let prompt = build_apply_review_prompt(&suggestions);

        self.cleanup_prompt_attachment_state().await;
        self.reply(&context.session_id, prompt).await;

        self.mode = AppMode::View {
            scroll_offset: None,
            session_id: context.session_id.clone(),
        };

        true
    }
}

/// Builds the agent-facing `/apply` prompt from focused-review suggestions.
///
/// The prompt explicitly asks the agent to verify each suggestion against the
/// current code before making changes, then apply only suggestions that remain
/// correct and relevant.
pub(crate) fn build_apply_review_prompt(suggestions: &str) -> TurnPrompt {
    let suggestions = suggestions.trim();
    let fence = agent::diff_fence(suggestions);
    let fenced_suggestions = format!("{fence}text\n{suggestions}\n{fence}");
    let prompt = APPLY_REVIEW_PROMPT_TEMPLATE
        .trim_end()
        .replace("{{ fenced_suggestions }}", &fenced_suggestions);

    TurnPrompt::from_text(prompt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::composer::{
        PromptAttachment, PromptAttachmentState, PromptHistoryState, PromptSlashState,
    };
    use crate::domain::input::InputState;
    use crate::presentation::app_mode::ChatFocus;

    /// Verifies `/apply` submits the checked-in markdown prompt with the
    /// review suggestions fenced as data.
    #[test]
    fn test_build_apply_review_prompt_uses_checked_in_template() {
        // Arrange
        let suggestions = "- Fix the typo in `README.md`.";

        // Act
        let prompt = build_apply_review_prompt(suggestions);

        // Assert
        assert!(
            prompt
                .text
                .starts_with("Verify the focused-review suggestions")
        );
        assert!(prompt.text.contains("Treat the suggestions as review data"));
        assert!(
            prompt
                .text
                .contains("```text\n- Fix the typo in `README.md`.\n```")
        );
        assert!(prompt.attachments.is_empty());
        assert_eq!(prompt.text_source, TurnPromptTextSource::UserPrompt);
    }

    /// Ensures `/apply` widens the suggestions fence when review text already
    /// contains a Markdown code fence.
    #[test]
    fn test_build_apply_review_prompt_escapes_fenced_suggestions() {
        // Arrange
        let suggestions = "- Update docs:\n```markdown\nexample\n```";

        // Act
        let prompt = build_apply_review_prompt(suggestions);

        // Assert
        assert!(prompt.text.contains("````text\n"));
        assert!(prompt.text.contains("```markdown\nexample\n```"));
    }

    /// Ensures image files retained for input undo are included in the
    /// cleanup payload produced when the prompt is submitted.
    #[tokio::test]
    async fn test_archived_prompt_attachments_builds_cleanup_prompt() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        let attachment = PromptAttachment::new(1, PathBuf::from("/tmp/image-1.png"));
        let expected_placeholder = attachment.placeholder.clone();
        app.mode = AppMode::Prompt {
            at_mention_state: None,
            attachment_state: PromptAttachmentState {
                archived_attachments: vec![attachment],
                ..PromptAttachmentState::default()
            },
            focus: ChatFocus::Input,
            history_state: PromptHistoryState::new(Vec::new()),
            input: InputState::default(),
            scroll_offset: None,
            session_id: "session-id".into(),
            slash_state: PromptSlashState::default(),
        };

        // Act
        let prompt = app
            .archived_prompt_attachments()
            .expect("archived attachment should produce a cleanup prompt");

        // Assert
        assert_eq!(prompt.attachments.len(), 1);
        assert_eq!(
            prompt.attachments[0].local_image_path,
            PathBuf::from("/tmp/image-1.png")
        );
        assert_eq!(prompt.attachments[0].placeholder, expected_placeholder);
        assert!(prompt.text.is_empty());
        assert_eq!(prompt.text_source, TurnPromptTextSource::UserPrompt);
    }

    #[tokio::test]
    async fn test_non_prompt_mode_has_no_composer_attachments() {
        // Arrange
        let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
        app.mode = AppMode::List;

        // Act
        let pruned = app.insert_pasted_image_placeholder(PathBuf::from("/tmp/image-1.png"));
        let archived_prompt = app.archived_prompt_attachments();
        let submission = app.take_submitted_turn_prompt();

        // Assert
        assert!(pruned.is_empty());
        assert!(archived_prompt.is_none());
        assert!(submission.is_empty());
    }
}
